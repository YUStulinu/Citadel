//! Page faults: demand-zero pages for user memory areas, stack growth, copy-on-write.
use crate::arch::{cpu, trap::TrapFrame};
use crate::mm::{frame, paging};
use crate::proc::{self, PROT_EXEC, PROT_READ, PROT_WRITE, STACK_MAX};
use kcore::elf::USER_TOP;

pub fn page_fault(f: &mut TrapFrame, addr: u64) {
    let write = f.error & 2 != 0;
    let present = f.error & 1 != 0;
    let instr = f.error & 16 != 0;
    if addr < USER_TOP && handle_user(addr, write, instr) {
        return;
    }
    if f.from_user() {
        crate::signal::force(crate::signal::SIGSEGV, f);
        return;
    }
    crate::kprintln!("kernel page fault: {:x?}", f);
    backtrace(f.rbp);
    panic!("kernel page fault at {:#x} (rip {:#x}, {}{})", addr, f.rip, if write { "write" } else { "read" }, if present { ", protection" } else { ", not present" });
}

/// Walks the frame-pointer chain and prints return addresses (resolve with addr2line).
pub fn backtrace(mut rbp: u64) {
    for _ in 0..16 {
        if rbp < 0xffff_8000_0000_0000 || rbp & 7 != 0 {
            break;
        }
        let ret = unsafe { *((rbp + 8) as *const u64) };
        crate::kprintln!("  at {:#x}", ret);
        rbp = unsafe { *(rbp as *const u64) };
    }
}

/// Makes `addr` accessible to the current process if its memory areas allow it. Returns false
/// for an access the program is not allowed to make.
pub fn handle_user(addr: u64, write: bool, exec: bool) -> bool {
    let Some(p) = proc::try_current() else { return false };
    let mut mm = p.mm.lock();
    let page = addr & !0xfff;
    // Grow the stack downwards (up to STACK_MAX) when the access is just below it.
    let mut prot = match mm.find(addr) {
        Some(v) => v.prot,
        None => {
            let grow = mm.vmas.iter_mut().find(|v| v.grows_down && addr < v.start && v.start - page <= 64 * 1024 && v.end - page <= STACK_MAX);
            match grow {
                Some(v) => {
                    v.start = page;
                    v.prot
                }
                None => return false,
            }
        }
    };
    if (write && prot & PROT_WRITE == 0) || (exec && prot & PROT_EXEC == 0) || (!write && !exec && prot & (PROT_READ | PROT_WRITE | PROT_EXEC) == 0) {
        return false;
    }
    if prot & PROT_WRITE != 0 {
        prot |= PROT_READ;
    }
    let cr3 = mm.cr3;
    match paging::entry(cr3, page) {
        None => {
            let Some(phys) = frame::alloc_zeroed(0) else { return false };
            let mut flags = paging::U;
            if prot & PROT_WRITE != 0 {
                flags |= paging::W;
            }
            if prot & PROT_EXEC == 0 {
                flags |= paging::NX;
            }
            if !paging::map(cr3, page, phys, flags) {
                frame::free(phys, 0);
                return false;
            }
            true
        }
        Some(e) => {
            if write && *e & paging::COW != 0 {
                let phys = *e & paging::ADDR;
                if frame::page_refs(phys) == 0 {
                    *e = (*e | paging::W) & !paging::COW;
                } else {
                    let Some(n) = frame::alloc(0) else { return false };
                    unsafe {
                        core::ptr::copy_nonoverlapping(crate::boot::phys_to_virt(phys) as *const u8, crate::boot::phys_to_virt(n) as *mut u8, 4096);
                    }
                    *e = n | ((*e & !paging::ADDR & !paging::COW) | paging::W);
                    frame::page_unref(phys);
                }
                cpu::invlpg(page);
                return true;
            }
            // Present and allowed: another CPU of this process fixed it meanwhile, or a stale
            // TLB entry.
            if write && *e & paging::W == 0 {
                return false;
            }
            cpu::invlpg(page);
            true
        }
    }
}
