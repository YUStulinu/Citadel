//! A buddy allocator for physical page frames.
//!
//! Blocks are 2^order frames (order 0 = 4 KiB, up to MAX_ORDER = 4 MiB). Each order has a
//! doubly linked free list whose links live inside the free blocks themselves (next at +0, prev
//! at +8), so the allocator needs no memory of its own besides one byte of metadata per frame:
//! bit 7 set = this frame starts a free block, low bits = the block's order. Freeing merges a
//! block with its buddy (the block at `addr ^ size`) as long as the buddy is free and of the
//! same order.
//!
//! Memory is accessed through [`Mem`] so that the same code runs in the kernel (through the
//! direct map) and in host tests (on a byte array).

pub const PAGE: u64 = 4096;
pub const MAX_ORDER: usize = 10;
const FREE: u8 = 0x80;
const NIL: u64 = u64::MAX;

/// Reads and writes 64-bit words of physical memory.
pub trait Mem {
    fn read(&self, phys: u64) -> u64;
    fn write(&mut self, phys: u64, v: u64);
}

pub struct Buddy<'a, M: Mem> {
    mem: M,
    base: u64,        // physical address of frame 0 of `meta`
    meta: &'a mut [u8],
    heads: [u64; MAX_ORDER + 1],
    free_frames: u64,
}

impl<'a, M: Mem> Buddy<'a, M> {
    /// `meta` has one byte per frame of the range starting at `base` that may ever be managed.
    pub fn new(mem: M, base: u64, meta: &'a mut [u8]) -> Self {
        meta.fill(0);
        Buddy { mem, base, meta, heads: [NIL; MAX_ORDER + 1], free_frames: 0 }
    }

    pub fn free_frames(&self) -> u64 {
        self.free_frames
    }

    pub fn mem(&mut self) -> &mut M {
        &mut self.mem
    }

    fn idx(&self, phys: u64) -> usize {
        ((phys - self.base) / PAGE) as usize
    }

    fn in_range(&self, phys: u64) -> bool {
        phys >= self.base && ((phys - self.base) / PAGE) < self.meta.len() as u64
    }

    fn push(&mut self, phys: u64, order: usize) {
        let head = self.heads[order];
        self.mem.write(phys, head);
        self.mem.write(phys + 8, NIL);
        if head != NIL {
            self.mem.write(head + 8, phys);
        }
        self.heads[order] = phys;
        let i = self.idx(phys);
        self.meta[i] = FREE | order as u8;
    }

    fn unlink(&mut self, phys: u64, order: usize) {
        let next = self.mem.read(phys);
        let prev = self.mem.read(phys + 8);
        if prev == NIL {
            self.heads[order] = next;
        } else {
            self.mem.write(prev, next);
        }
        if next != NIL {
            self.mem.write(next + 8, prev);
        }
        let i = self.idx(phys);
        self.meta[i] = 0;
    }

    /// Adds [start, end) (page-aligned inward) to the free memory.
    pub fn add_region(&mut self, start: u64, end: u64) {
        let mut a = (start + PAGE - 1) & !(PAGE - 1);
        let end = end & !(PAGE - 1);
        while a < end {
            // The largest aligned block that fits.
            let mut order = MAX_ORDER;
            while order > 0 && ((a - self.base) % (PAGE << order) != 0 || a + (PAGE << order) > end) {
                order -= 1;
            }
            if !self.in_range(a) || !self.in_range(a + (PAGE << order) - 1) {
                return;
            }
            self.free(a, order);
            a += PAGE << order;
        }
    }

    /// Allocates 2^order contiguous frames.
    pub fn alloc(&mut self, order: usize) -> Option<u64> {
        let mut o = order;
        while o <= MAX_ORDER && self.heads[o] == NIL {
            o += 1;
        }
        if o > MAX_ORDER {
            return None;
        }
        let block = self.heads[o];
        self.unlink(block, o);
        // Split down to the requested order, freeing the upper halves.
        while o > order {
            o -= 1;
            self.push(block + (PAGE << o), o);
        }
        self.free_frames -= 1 << order;
        Some(block)
    }

    /// Frees a block previously allocated with the same order.
    pub fn free(&mut self, phys: u64, order: usize) {
        let mut block = phys;
        let mut o = order;
        self.free_frames += 1 << order;
        while o < MAX_ORDER {
            let buddy = self.base + ((block - self.base) ^ (PAGE << o));
            if !self.in_range(buddy) || self.meta[self.idx(buddy)] != FREE | o as u8 {
                break;
            }
            self.unlink(buddy, o);
            block = block.min(buddy);
            o += 1;
        }
        self.push(block, o);
    }

    /// Number of free blocks of each order (for /proc/buddyinfo and tests).
    pub fn counts(&self) -> [u64; MAX_ORDER + 1] {
        let mut out = [0; MAX_ORDER + 1];
        for (o, c) in out.iter_mut().enumerate() {
            let mut p = self.heads[o];
            while p != NIL {
                *c += 1;
                p = self.mem.read(p);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Fake(HashMap<u64, u64>);
    impl Mem for Fake {
        fn read(&self, p: u64) -> u64 {
            *self.0.get(&p).unwrap_or(&0)
        }
        fn write(&mut self, p: u64, v: u64) {
            self.0.insert(p, v);
        }
    }

    const BASE: u64 = 0x100000;

    #[test]
    fn allocates_splits_and_merges_back() {
        let mut meta = vec![0u8; 4096];
        let mut b = Buddy::new(Fake::default(), BASE, &mut meta);
        b.add_region(BASE, BASE + 2048 * PAGE);
        assert_eq!(b.free_frames(), 2048);
        assert_eq!(b.counts()[MAX_ORDER], 2);
        let a = b.alloc(0).unwrap();
        assert!(a == BASE || a == BASE + 1024 * PAGE, "{a:#x}");
        assert_eq!(b.free_frames(), 2047);
        // The split leaves one free block at every order below MAX_ORDER.
        let c = b.counts();
        assert!((0..MAX_ORDER).all(|o| c[o] == 1), "{c:?}");
        b.free(a, 0);
        assert_eq!(b.counts()[MAX_ORDER], 2);
        assert_eq!(b.free_frames(), 2048);
    }

    #[test]
    fn huge_block_freed_in_small_pieces_merges_back() {
        // A 2 MiB block split into 4 KiB pages (as when a huge page is broken up) and freed page
        // by page must end up as free large blocks again.
        let mut meta = vec![0u8; 4096];
        let mut b = Buddy::new(Fake::default(), BASE, &mut meta);
        b.add_region(BASE, BASE + 2048 * PAGE);
        let blk = b.alloc(9).unwrap();
        assert_eq!(blk % (512 * PAGE), BASE % (512 * PAGE));
        for i in 0..512 {
            b.free(blk + i * PAGE, 0);
        }
        assert_eq!(b.free_frames(), 2048);
        assert_eq!(b.counts()[MAX_ORDER], 2);
    }

    #[test]
    fn unaligned_regions_and_exhaustion() {
        let mut meta = vec![0u8; 4096];
        let mut b = Buddy::new(Fake::default(), BASE, &mut meta);
        b.add_region(BASE + 3 * PAGE + 17, BASE + 40 * PAGE + 5);
        assert_eq!(b.free_frames(), 36); // frames 4..39
        let mut got = std::collections::HashSet::new();
        while let Some(p) = b.alloc(0) {
            assert!(p >= BASE + 4 * PAGE && p < BASE + 40 * PAGE);
            assert!(got.insert(p), "frame handed out twice");
        }
        assert_eq!(got.len(), 36);
        assert!(b.alloc(0).is_none());
        for p in got {
            b.free(p, 0);
        }
        assert_eq!(b.free_frames(), 36);
    }

    #[test]
    fn random_workload_never_overlaps() {
        let mut meta = vec![0u8; 8192];
        let mut b = Buddy::new(Fake::default(), BASE, &mut meta);
        b.add_region(BASE, BASE + 8192 * PAGE);
        let mut live: Vec<(u64, usize)> = Vec::new();
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20000 {
            if rnd() % 3 != 0 || live.is_empty() {
                let order = (rnd() % 6) as usize;
                if let Some(p) = b.alloc(order) {
                    assert_eq!((p - BASE) % (PAGE << order), 0, "misaligned block");
                    let end = p + (PAGE << order);
                    for &(q, qo) in &live {
                        let qend = q + (PAGE << qo);
                        assert!(end <= q || p >= qend, "overlap");
                    }
                    live.push((p, order));
                }
            } else {
                let i = (rnd() as usize) % live.len();
                let (p, o) = live.swap_remove(i);
                b.free(p, o);
            }
        }
        for (p, o) in live.drain(..) {
            b.free(p, o);
        }
        assert_eq!(b.free_frames(), 8192);
        assert_eq!(b.counts()[MAX_ORDER], 8);
    }
}
