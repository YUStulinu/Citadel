//! execve: replaces a process's memory with a program from the file system.
//!
//! Static ELF executables are loaded eagerly (every segment page is allocated and filled); a
//! "#!" script runs its interpreter. The new stack holds argc, argv, envp and the auxiliary
//! vector the way the System V ABI (and Linux) lays them out, so a static binary built for
//! Linux starts the same way here.
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use kcore::elf;
use kcore::errno::*;

use crate::arch::trap::TrapFrame;
use crate::boot::phys_to_virt;
use crate::fs::{self, Kind};
use crate::mm::{frame, paging};
use crate::proc::{PROT_EXEC, PROT_READ, PROT_WRITE, Process, STACK_TOP, Vma};

const STACK_EAGER: u64 = 32 * 1024;

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_HWCAP: u64 = 16;
const AT_CLKTCK: u64 = 17;
const AT_RANDOM: u64 = 25;
const AT_EXECFN: u64 = 31;

pub fn exec(p: &Arc<Process>, path: &str, argv: &[String], envp: &[String], frame: &mut TrapFrame) -> KResult<()> {
    exec_depth(p, path, argv, envp, frame, 0)
}

fn exec_depth(p: &Arc<Process>, path: &str, argv: &[String], envp: &[String], frame: &mut TrapFrame, depth: u32) -> KResult<()> {
    let abs = kcore::path::normalize(&p.cwd.lock(), path);
    let (_, inode) = fs::resolve(&abs, true)?;
    if inode.kind() != Kind::File {
        return Err(if inode.kind() == Kind::Dir { EISDIR } else { EACCES });
    }
    if inode.stat().mode & 0o111 == 0 {
        return Err(EACCES);
    }
    let image = fs::read_all(&inode)?;
    if image.starts_with(b"#!") {
        if depth > 4 {
            return Err(ELOOP);
        }
        let line_end = image.iter().position(|&c| c == b'\n').unwrap_or(image.len()).min(256);
        let line = core::str::from_utf8(&image[2..line_end]).map_err(|_| ENOEXEC)?.trim();
        let mut parts = line.splitn(2, char::is_whitespace);
        let interp = parts.next().filter(|s| !s.is_empty()).ok_or(ENOEXEC)?;
        let mut nargv = Vec::new();
        nargv.push(String::from(interp));
        if let Some(a) = parts.next().map(str::trim).filter(|s| !s.is_empty()) {
            nargv.push(String::from(a));
        }
        nargv.push(abs.clone());
        nargv.extend(argv.iter().skip(1).cloned());
        return exec_depth(p, interp, &nargv, envp, frame, depth + 1);
    }
    let e = elf::parse(&image).map_err(|_| ENOEXEC)?;

    // Build the new address space completely before touching the old one.
    let cr3 = paging::new_address_space().ok_or(ENOMEM)?;
    let mut vmas = Vec::new();
    let mut brk = 0;
    let fail = |cr3| {
        paging::free_address_space(cr3);
        ENOMEM
    };
    for s in e.segments.iter().flatten() {
        let start = s.vaddr & !0xfff;
        let end = (s.vaddr + s.memsz + 0xfff) & !0xfff;
        let mut flags = paging::U;
        if s.writable {
            flags |= paging::W;
        }
        if !s.executable {
            flags |= paging::NX;
        }
        let mut page = start;
        while page < end {
            // Segments may share a page (the end of text and the start of data).
            let phys = match paging::translate(cr3, page) {
                Some(ph) => {
                    if s.writable {
                        if let Some(en) = paging::entry(cr3, page) {
                            *en |= paging::W;
                        }
                    }
                    ph & paging::ADDR
                }
                None => {
                    let Some(ph) = frame::alloc_zeroed(0) else { return Err(fail(cr3)) };
                    if !paging::map(cr3, page, ph, flags) {
                        frame::free(ph, 0);
                        return Err(fail(cr3));
                    }
                    ph
                }
            };
            // The part of the file that lands in this page.
            let lo = page.max(s.vaddr);
            let hi = (page + 4096).min(s.vaddr + s.filesz);
            if lo < hi {
                let src = &image[(s.offset + (lo - s.vaddr)) as usize..(s.offset + (hi - s.vaddr)) as usize];
                unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), (phys_to_virt(phys) + (lo - page)) as *mut u8, src.len()) };
            }
            page += 4096;
        }
        let prot = (if s.readable { PROT_READ } else { 0 }) | (if s.writable { PROT_WRITE } else { 0 }) | (if s.executable { PROT_EXEC } else { 0 });
        vmas.push(Vma { start, end, prot, grows_down: false });
        brk = brk.max(end);
    }

    // The stack: the top STACK_EAGER bytes now (they hold the arguments), more on demand.
    let stack_low = STACK_TOP - STACK_EAGER;
    let mut stack_frames = Vec::new();
    let mut page = stack_low;
    while page < STACK_TOP {
        let Some(ph) = frame::alloc_zeroed(0) else { return Err(fail(cr3)) };
        if !paging::map(cr3, page, ph, paging::U | paging::W | paging::NX) {
            return Err(fail(cr3));
        }
        stack_frames.push(ph);
        page += 4096;
    }
    vmas.push(Vma { start: stack_low, end: STACK_TOP, prot: PROT_READ | PROT_WRITE, grows_down: true });

    // Lay out the initial stack in a buffer that mirrors [stack_low, STACK_TOP).
    let mut buf = alloc::vec![0u8; STACK_EAGER as usize];
    let mut top = STACK_EAGER as usize;
    let mut push_bytes = |b: &[u8], top: &mut usize| -> KResult<u64> {
        if *top < b.len() + 1024 {
            return Err(E2BIG);
        }
        *top -= b.len();
        buf[*top..*top + b.len()].copy_from_slice(b);
        Ok(stack_low + *top as u64)
    };
    let mut random = [0u8; 16];
    fs::devfs::fill_random(&mut random);
    let random_at = push_bytes(&random, &mut top)?;
    let mut push_str = |s: &str, top: &mut usize| -> KResult<u64> {
        let mut v = Vec::with_capacity(s.len() + 1);
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        push_bytes(&v, top)
    };
    let execfn = push_str(&abs, &mut top)?;
    let mut env_ptrs = Vec::new();
    for s in envp {
        env_ptrs.push(push_str(s, &mut top)?);
    }
    let mut arg_ptrs = Vec::new();
    for s in argv {
        arg_ptrs.push(push_str(s, &mut top)?);
    }
    let aux = [
        (AT_PHDR, e.phdr_vaddr),
        (AT_PHENT, e.phent as u64),
        (AT_PHNUM, e.phnum as u64),
        (AT_PAGESZ, 4096),
        (AT_ENTRY, e.entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_HWCAP, crate::arch::cpu::cpuid(1, 0).edx as u64),
        (AT_CLKTCK, 100),
        (AT_RANDOM, random_at),
        (AT_EXECFN, execfn),
        (AT_NULL, 0),
    ];
    let words = 1 + arg_ptrs.len() + 1 + env_ptrs.len() + 1 + aux.len() * 2;
    top &= !15;
    if (words * 8) % 16 != 0 {
        top -= 8;
    }
    if top < words * 8 + 256 {
        return Err(E2BIG);
    }
    top -= words * 8;
    let sp = stack_low + top as u64;
    let mut w = top;
    let mut put = |v: u64, w: &mut usize| {
        buf[*w..*w + 8].copy_from_slice(&v.to_le_bytes());
        *w += 8;
    };
    put(arg_ptrs.len() as u64, &mut w);
    for a in &arg_ptrs {
        put(*a, &mut w);
    }
    put(0, &mut w);
    for e in &env_ptrs {
        put(*e, &mut w);
    }
    put(0, &mut w);
    for (k, v) in aux {
        put(k, &mut w);
        put(v, &mut w);
    }
    for (i, ph) in stack_frames.iter().enumerate() {
        unsafe { core::ptr::copy_nonoverlapping(buf[i * 4096..].as_ptr(), phys_to_virt(*ph) as *mut u8, 4096) };
    }

    // Commit: switch the process to the new image.
    let brk = (brk + 0xfff) & !0xfff;
    {
        let mut mm = p.mm.lock();
        let old = mm.cr3;
        mm.cr3 = cr3;
        mm.vmas = vmas;
        mm.brk_start = brk;
        mm.brk = brk;
        mm.mmap_next = crate::proc::MMAP_TOP;
        let t = crate::sched::current();
        t.cr3.store(cr3, Ordering::Release);
        crate::arch::cpu::write_cr3(cr3);
        if old != paging::kernel_pml4() {
            paging::free_address_space(old);
        }
    }
    {
        let mut fds = p.fds.lock();
        for e in fds.fds.iter_mut() {
            if e.as_ref().is_some_and(|e| e.cloexec) {
                *e = None;
            }
        }
    }
    p.sig.lock().reset_on_exec();
    *p.name.lock() = String::from(kcore::path::split_last(&abs).1);
    crate::arch::cpu::wrmsr(crate::arch::cpu::MSR_FS_BASE, 0);
    crate::sched::current().fs_base.store(0, Ordering::Relaxed);
    *frame = TrapFrame::user(e.entry, sp);
    frame.vector = 0; // leave through iretq (all registers are fresh)
    Ok(())
}
