//! Memory system calls: brk, mmap (anonymous, and private file mappings read eagerly),
//! munmap, mprotect.
use kcore::errno::*;

use crate::fs::Kind;
use crate::mm::paging;
use crate::proc::{self, PROT_EXEC, PROT_READ, PROT_WRITE, Vma};

const MAP_SHARED: u32 = 1;
const MAP_PRIVATE: u32 = 2;
const MAP_FIXED: u32 = 0x10;
const MAP_ANONYMOUS: u32 = 0x20;
const MAP_FIXED_NOREPLACE: u32 = 0x100000;

fn page_up(x: u64) -> u64 {
    (x + 0xfff) & !0xfff
}

pub fn brk(addr: u64) -> KResult<u64> {
    let p = proc::current();
    let mut mm = p.mm.lock();
    if addr == 0 || addr < mm.brk_start {
        return Ok(mm.brk);
    }
    let new_end = page_up(addr);
    let old_end = page_up(mm.brk);
    if new_end > old_end {
        // Must not run into another area.
        if mm.vmas.iter().any(|v| v.start < new_end && v.end > old_end && v.start >= mm.brk_start) {
            return Ok(mm.brk);
        }
        let start = mm.brk_start;
        // Extend (or create) the heap area.
        match mm.vmas.iter_mut().find(|v| v.start == start) {
            Some(v) => v.end = new_end,
            None => {
                mm.vmas.push(Vma { start, end: new_end, prot: PROT_READ | PROT_WRITE, grows_down: false, nohuge: false });
                mm.vmas.sort_by_key(|v| v.start);
            }
        }
    } else if new_end < old_end {
        let cr3 = mm.cr3;
        let start = mm.brk_start;
        if let Some(v) = mm.vmas.iter_mut().find(|v| v.start == start) {
            v.end = new_end.max(start);
        }
        paging::unmap_range(cr3, new_end, old_end);
    }
    mm.brk = addr;
    Ok(addr)
}

pub fn mmap(addr: u64, len: u64, prot: u32, flags: u32, fd: i64, off: u64) -> KResult<u64> {
    if len == 0 || off & 0xfff != 0 {
        return Err(EINVAL);
    }
    let len = page_up(len);
    if len > 1 << 40 {
        return Err(ENOMEM);
    }
    if flags & (MAP_SHARED | MAP_PRIVATE) == 0 {
        return Err(EINVAL);
    }
    // File contents (copied: a private mapping, or a shared one nobody else writes to).
    let data = if flags & MAP_ANONYMOUS == 0 {
        let f = proc::current().fds.lock().get(fd)?;
        if f.inode.kind() != Kind::File {
            return Err(ENODEV);
        }
        let size = f.inode.stat().size;
        let n = len.min(size.saturating_sub(off)) as usize;
        let mut buf = alloc::vec![0u8; n];
        let mut done = 0;
        while done < n {
            let r = f.inode.read_at(off + done as u64, &mut buf[done..])?;
            if r == 0 {
                break;
            }
            done += r;
        }
        Some(buf)
    } else {
        None
    };
    let p = proc::current();
    let start = {
        let mut mm = p.mm.lock();
        let start = if flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0 {
            if addr & 0xfff != 0 || addr + len > kcore::elf::USER_TOP {
                return Err(EINVAL);
            }
            if flags & MAP_FIXED_NOREPLACE != 0 && mm.vmas.iter().any(|v| v.start < addr + len && v.end > addr) {
                return Err(EEXIST);
            }
            addr
        } else {
            mm.find_free(len)
        };
        // Writable while we fill it; the final protection is set below.
        mm.insert(Vma { start, end: start + len, prot: prot | if data.is_some() { PROT_WRITE } else { 0 }, grows_down: false, nohuge: false });
        start
    };
    if let Some(d) = data {
        crate::user::copy_out(start, &d)?;
        if prot & PROT_WRITE == 0 {
            mprotect(start, len, prot)?;
        }
    }
    Ok(start)
}

pub fn munmap(addr: u64, len: u64) -> KResult<u64> {
    if addr & 0xfff != 0 || len == 0 {
        return Err(EINVAL);
    }
    let p = proc::current();
    p.mm.lock().remove_range(addr, addr + page_up(len));
    Ok(0)
}

pub fn mprotect(addr: u64, len: u64, prot: u32) -> KResult<u64> {
    if addr & 0xfff != 0 {
        return Err(EINVAL);
    }
    let end = addr + page_up(len);
    let p = proc::current();
    let mut mm = p.mm.lock();
    // Split the areas at the edges, then change those inside.
    let mut out = alloc::vec::Vec::new();
    let mut covered = 0;
    for v in mm.vmas.drain(..) {
        if v.end <= addr || v.start >= end {
            out.push(v);
            continue;
        }
        if v.start < addr {
            out.push(Vma { end: addr, ..v.clone() });
        }
        let (s, e) = (v.start.max(addr), v.end.min(end));
        covered += e - s;
        out.push(Vma { start: s, end: e, prot, grows_down: v.grows_down, nohuge: v.nohuge });
        if v.end > end {
            out.push(Vma { start: end, ..v.clone() });
        }
    }
    out.sort_by_key(|v| v.start);
    mm.vmas = out;
    if covered < end - addr {
        return Err(ENOMEM);
    }
    // Update the pages already present (COW pages stay read-only until written).
    let cr3 = mm.cr3;
    let mut a = addr;
    while a < end {
        if let Some(e) = paging::entry(cr3, a) {
            let mut v = *e & !(paging::W | paging::NX);
            if prot & PROT_WRITE != 0 && v & paging::COW == 0 {
                v |= paging::W;
            }
            if prot & PROT_EXEC == 0 {
                v |= paging::NX;
            }
            *e = v;
            crate::sched::user_tlb_changed(a);
        }
        a += 4096;
    }
    Ok(0)
}

/// madvise: MADV_HUGEPAGE / MADV_NOHUGEPAGE switch transparent huge pages for the areas in the
/// range; other advice is accepted and ignored.
pub fn madvise(addr: u64, len: u64, advice: u32) -> KResult<u64> {
    const MADV_HUGEPAGE: u32 = 14;
    const MADV_NOHUGEPAGE: u32 = 15;
    if addr & 0xfff != 0 {
        return Err(EINVAL);
    }
    if advice == MADV_HUGEPAGE || advice == MADV_NOHUGEPAGE {
        let p = crate::proc::current();
        let mut mm = p.mm.lock();
        let end = addr.saturating_add(len);
        for v in mm.vmas.iter_mut().filter(|v| v.start < end && v.end > addr) {
            v.nohuge = advice == MADV_NOHUGEPAGE;
        }
    }
    Ok(0)
}
