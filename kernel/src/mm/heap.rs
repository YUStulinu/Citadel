//! The kernel heap: size classes from 16 to 2048 bytes carved out of pages, and whole buddy
//! blocks for anything larger. Every block is reached through the direct map.
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;

use kcore::buddy::PAGE;

use super::frame;
use crate::boot::{phys_to_virt, virt_to_phys};
use crate::sync::SpinLock;

const CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

struct Class {
    free: *mut u8, // singly linked through the first word of each free block
    allocated: usize,
}
unsafe impl Send for Class {}

pub struct Heap {
    classes: [SpinLock<Class>; 8],
}

const fn class() -> SpinLock<Class> {
    SpinLock::new(Class { free: null_mut(), allocated: 0 })
}

#[global_allocator]
pub static HEAP: Heap = Heap { classes: [class(), class(), class(), class(), class(), class(), class(), class()] };

fn class_of(size: usize) -> Option<usize> {
    CLASSES.iter().position(|&c| c >= size)
}

fn order_of(size: usize) -> usize {
    let pages = size.div_ceil(PAGE as usize);
    pages.next_power_of_two().trailing_zeros() as usize
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let size = l.size().max(l.align()).max(8);
        match class_of(size) {
            Some(i) => {
                let mut c = self.classes[i].lock();
                if c.free.is_null() {
                    // Refill with one page cut into blocks (blocks of a power-of-two size are
                    // aligned to their size within the page).
                    let Some(p) = frame::alloc(0) else { return null_mut() };
                    let base = phys_to_virt(p) as *mut u8;
                    let bs = CLASSES[i];
                    for k in (0..PAGE as usize / bs).rev() {
                        let b = unsafe { base.add(k * bs) };
                        unsafe { *(b as *mut *mut u8) = c.free };
                        c.free = b;
                    }
                }
                let b = c.free;
                c.free = unsafe { *(b as *mut *mut u8) };
                c.allocated += 1;
                b
            }
            None => match frame::alloc(order_of(size)) {
                Some(p) => phys_to_virt(p) as *mut u8,
                None => null_mut(),
            },
        }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        let size = l.size().max(l.align()).max(8);
        match class_of(size) {
            Some(i) => {
                let mut c = self.classes[i].lock();
                unsafe { *(p as *mut *mut u8) = c.free };
                c.free = p;
                c.allocated -= 1;
            }
            None => frame::free(virt_to_phys(p as u64), order_of(size)),
        }
    }
}

/// Blocks in use per size class (for /proc/meminfo).
pub fn stats() -> [(usize, usize); 8] {
    let mut out = [(0, 0); 8];
    for (i, c) in HEAP.classes.iter().enumerate() {
        out[i] = (CLASSES[i], c.lock().allocated);
    }
    out
}
