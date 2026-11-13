//! Four-level page tables.
//!
//! The upper half (PML4 entries 256..511) is the kernel's and is identical in every address
//! space: all its PDPTs are allocated at boot, so later kernel mappings show up everywhere
//! without touching each process. The lower half belongs to the process.
use alloc::vec::Vec;

use super::frame;
use crate::arch::cpu;
use crate::boot::phys_to_virt;
use crate::sync::{Once, SpinLock};
use kcore::buddy::PAGE;

pub const P: u64 = 1;
pub const W: u64 = 1 << 1;
pub const U: u64 = 1 << 2;
pub const PWT: u64 = 1 << 3;
pub const PCD: u64 = 1 << 4;
pub const HUGE: u64 = 1 << 7;
pub const G: u64 = 1 << 8;
pub const COW: u64 = 1 << 9; // software: read-only now, copy on the first write
pub const NX: u64 = 1 << 63;
pub const ADDR: u64 = 0x000f_ffff_ffff_f000;

/// MMIO mappings go here (PML4 entry 510), uncached.
const MMIO_BASE: u64 = 0xffff_ff00_0000_0000;

fn table(phys: u64) -> &'static mut [u64; 512] {
    unsafe { &mut *(phys_to_virt(phys) as *mut [u64; 512]) }
}

fn index(v: u64, level: u32) -> usize {
    ((v >> (12 + 9 * level)) & 511) as usize
}

static KERNEL_PML4: Once<u64> = Once::new();
static MMIO_NEXT: SpinLock<u64> = SpinLock::new(MMIO_BASE);

/// Builds the kernel's own PML4 from the loader's upper half and switches to it.
pub fn init() {
    let boot = cpu::read_cr3() & ADDR;
    let pml4 = frame::alloc_zeroed(0).expect("no memory for the kernel PML4");
    let (src, dst) = (table(boot), table(pml4));
    for i in 256..512 {
        dst[i] = src[i];
        if dst[i] & P == 0 {
            dst[i] = frame::alloc_zeroed(0).expect("no memory") | P | W;
        }
    }
    KERNEL_PML4.set(pml4);
    cpu::write_cr3(pml4);
}

pub fn kernel_pml4() -> u64 {
    *KERNEL_PML4.get()
}

/// The entry for `v` in the last-level table, creating intermediate tables if asked.
fn walk(pml4: u64, v: u64, create: bool, user: bool) -> Option<&'static mut u64> {
    let mut t = pml4;
    for level in (1..4).rev() {
        let e = &mut table(t)[index(v, level)];
        if *e & P == 0 {
            if !create {
                return None;
            }
            let n = frame::alloc_zeroed(0)?;
            *e = n | P | W | if user { U } else { 0 };
        } else if *e & HUGE != 0 {
            return None; // inside a 2 MiB/1 GiB page: not walked further
        }
        t = *e & ADDR;
    }
    Some(&mut table(t)[index(v, 0)])
}

/// Maps one 4 KiB page. Returns false when a table could not be allocated.
pub fn map(pml4: u64, v: u64, phys: u64, flags: u64) -> bool {
    match walk(pml4, v, true, flags & U != 0) {
        Some(e) => {
            *e = (phys & ADDR) | flags | P;
            true
        }
        None => false,
    }
}

/// The page table entry for v, if mapped.
pub fn entry(pml4: u64, v: u64) -> Option<&'static mut u64> {
    walk(pml4, v, false, false).filter(|e| **e & P != 0)
}

pub fn translate(pml4: u64, v: u64) -> Option<u64> {
    entry(pml4, v).map(|e| (*e & ADDR) | (v & (PAGE - 1)))
}

/// Unmaps a page and returns what it pointed to.
pub fn unmap(pml4: u64, v: u64) -> Option<u64> {
    let e = entry(pml4, v)?;
    let old = *e;
    *e = 0;
    if cpu::read_cr3() & ADDR == pml4 {
        cpu::invlpg(v);
    }
    Some(old)
}

/// Maps device memory (uncached) and returns its virtual address.
pub fn map_mmio(phys: u64, len: u64) -> u64 {
    let start = phys & !(PAGE - 1);
    let pages = (phys + len - start).div_ceil(PAGE);
    let mut next = MMIO_NEXT.lock();
    let v = *next;
    *next += pages * PAGE;
    for i in 0..pages {
        assert!(map(kernel_pml4(), v + i * PAGE, start + i * PAGE, W | PCD | PWT | G | NX), "MMIO mapping failed");
    }
    v + (phys - start)
}

/// A new address space: empty lower half, the kernel's upper half.
pub fn new_address_space() -> Option<u64> {
    let p = frame::alloc_zeroed(0)?;
    let (k, t) = (table(kernel_pml4()), table(p));
    t[256..].copy_from_slice(&k[256..]);
    Some(p)
}

/// Copies the lower half for fork: page tables are duplicated, data pages are shared
/// copy-on-write (writable pages become read-only + COW in both spaces, and gain a reference).
pub fn fork_address_space(src: u64) -> Option<u64> {
    let dst = new_address_space()?;
    fn copy(level: u32, s: u64, d: u64) -> bool {
        let (st, dt) = (table(s), table(d));
        let range = if level == 3 { 0..256 } else { 0..512 };
        for i in range {
            let e = st[i];
            if e & P == 0 {
                continue;
            }
            if level == 0 {
                let mut ne = e;
                if e & (W | COW) != 0 {
                    ne = (e & !W) | COW;
                    st[i] = ne;
                }
                frame::page_ref(e & ADDR);
                dt[i] = ne;
            } else {
                let Some(n) = frame::alloc_zeroed(0) else { return false };
                dt[i] = n | (e & !ADDR);
                if !copy(level - 1, e & ADDR, n) {
                    return false;
                }
            }
        }
        true
    }
    if !copy(3, src, dst) {
        free_address_space(dst);
        return None;
    }
    // The parent's writable pages just became read-only: flush its TLB.
    if cpu::read_cr3() & ADDR == src {
        cpu::write_cr3(src);
    }
    Some(dst)
}

/// Frees the lower half (pages and tables) and the PML4 itself.
pub fn free_address_space(pml4: u64) {
    fn free_level(level: u32, t: u64) {
        let tt = table(t);
        let range = if level == 3 { 0..256 } else { 0..512 };
        for i in range {
            let e = tt[i];
            if e & P == 0 {
                continue;
            }
            if level == 0 {
                frame::page_unref(e & ADDR);
            } else {
                free_level(level - 1, e & ADDR);
                frame::free(e & ADDR, 0);
            }
        }
    }
    free_level(3, pml4);
    frame::free(pml4, 0);
}

/// Unmaps and releases the user pages in [start, end).
pub fn unmap_range(pml4: u64, start: u64, end: u64) {
    let mut v = start;
    while v < end {
        if let Some(old) = unmap(pml4, v) {
            if old & P != 0 {
                frame::page_unref(old & ADDR);
            }
        }
        v += PAGE;
    }
}

/// Mapped user pages in the lower half (for /proc and tests).
pub fn user_pages(pml4: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    fn walk_all(level: u32, t: u64, base: u64, out: &mut Vec<(u64, u64)>) {
        let tt = table(t);
        let range = if level == 3 { 0..256 } else { 0..512 };
        for i in range {
            let e = tt[i];
            if e & P == 0 {
                continue;
            }
            let v = base | ((i as u64) << (12 + 9 * level));
            if level == 0 {
                out.push((v, e));
            } else {
                walk_all(level - 1, e & ADDR, v, out);
            }
        }
    }
    walk_all(3, pml4, 0, &mut out);
    out
}
