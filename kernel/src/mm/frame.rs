//! Physical memory: one buddy allocator over all usable RAM, behind a spinlock.
use kcore::buddy::{Buddy, Mem, PAGE};

use crate::boot::{BootInfo, phys_to_virt};
use crate::sync::{Once, SpinLock};

pub struct DirectMem;

impl Mem for DirectMem {
    fn read(&self, phys: u64) -> u64 {
        unsafe { core::ptr::read_volatile(phys_to_virt(phys) as *const u64) }
    }
    fn write(&mut self, phys: u64, v: u64) {
        unsafe { core::ptr::write_volatile(phys_to_virt(phys) as *mut u64, v) }
    }
}

static FRAMES: Once<SpinLock<Buddy<'static, DirectMem>>> = Once::new();
static mut TOTAL: u64 = 0;

/// Reference counts of user pages (copy-on-write sharing after fork), one u16 per frame.
static mut REFS: &mut [core::sync::atomic::AtomicU16] = &mut [];
static mut REFS_BASE: u64 = 0;

/// Builds the allocator from the boot memory map. Its metadata (one byte per frame) is carved
/// out of the first usable region large enough to hold it.
pub fn init(bi: &BootInfo) {
    let usable = || bi.regions().iter().filter(|r| r.kind == 1 && r.len >= PAGE);
    let lo = usable().map(|r| r.base).min().expect("no usable memory");
    let hi = usable().map(|r| r.base + r.len).max().unwrap();
    let lo = lo & !(PAGE - 1);
    let frames = ((hi - lo) / PAGE) as usize;
    let meta_bytes = (frames as u64 * 3 + PAGE - 1) & !(PAGE - 1); // buddy byte + u16 refcount
    let meta_region = usable().find(|r| r.base >= 0x100000 && r.len >= meta_bytes + PAGE).expect("no room for frame metadata");
    let meta_phys = (meta_region.base + PAGE - 1) & !(PAGE - 1);
    let meta = unsafe { core::slice::from_raw_parts_mut(phys_to_virt(meta_phys) as *mut u8, frames) };
    unsafe {
        let refs = phys_to_virt(meta_phys + ((frames as u64 + 1) & !1)) as *mut core::sync::atomic::AtomicU16;
        core::ptr::write_bytes(refs, 0, frames);
        REFS = core::slice::from_raw_parts_mut(refs, frames);
        REFS_BASE = lo;
    }
    let mut b = Buddy::new(DirectMem, lo, meta);
    let mut total = 0;
    for r in usable() {
        let mut start = r.base;
        let end = r.base + r.len;
        if start < 0x100000 {
            start = 0x100000; // keep low memory for the AP trampoline and firmware quirks
        }
        if start <= meta_phys && meta_phys < end {
            // Skip the metadata itself.
            if start < meta_phys {
                b.add_region(start, meta_phys);
            }
            start = meta_phys + meta_bytes;
        }
        if start < end {
            b.add_region(start, end);
        }
    }
    total += b.free_frames() * PAGE;
    unsafe { TOTAL = total };
    FRAMES.set(SpinLock::new(b));
}

/// Allocates 2^order physically contiguous, zeroed frames.
pub fn alloc_zeroed(order: usize) -> Option<u64> {
    let p = FRAMES.get().lock().alloc(order)?;
    unsafe { core::ptr::write_bytes(phys_to_virt(p) as *mut u8, 0, (PAGE << order) as usize) };
    Some(p)
}

/// Allocates 2^order frames without clearing them.
pub fn alloc(order: usize) -> Option<u64> {
    FRAMES.get().lock().alloc(order)
}

pub fn free(phys: u64, order: usize) {
    FRAMES.get().lock().free(phys, order)
}

pub fn free_bytes() -> u64 {
    FRAMES.get().lock().free_frames() * PAGE
}

pub fn total_bytes() -> u64 {
    unsafe { TOTAL }
}

pub fn order_counts() -> [u64; kcore::buddy::MAX_ORDER + 1] {
    FRAMES.get().lock().counts()
}

fn ref_slot(phys: u64) -> &'static core::sync::atomic::AtomicU16 {
    unsafe {
        let refs = &*core::ptr::addr_of!(REFS);
        &refs[((phys - REFS_BASE) / PAGE) as usize]
    }
}

/// One more mapping shares this user page.
pub fn page_ref(phys: u64) {
    ref_slot(phys).fetch_add(1, core::sync::atomic::Ordering::AcqRel);
}

/// Count of extra sharers (0 = only one owner).
pub fn page_refs(phys: u64) -> u16 {
    ref_slot(phys).load(core::sync::atomic::Ordering::Acquire)
}

/// Drops one mapping of a user page; frees the frame when it was the last.
pub fn page_unref(phys: u64) {
    let slot = ref_slot(phys);
    let mut cur = slot.load(core::sync::atomic::Ordering::Acquire);
    loop {
        if cur == 0 {
            free(phys, 0);
            return;
        }
        match slot.compare_exchange(cur, cur - 1, core::sync::atomic::Ordering::AcqRel, core::sync::atomic::Ordering::Acquire) {
            Ok(_) => return,
            Err(c) => cur = c,
        }
    }
}
