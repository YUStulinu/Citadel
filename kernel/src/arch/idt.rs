//! The interrupt descriptor table (one, shared by all CPUs).
use core::arch::asm;

use super::{gdt, trap};

#[repr(C)]
#[derive(Clone, Copy)]
struct Gate {
    off_lo: u16,
    sel: u16,
    ist: u8,
    attr: u8,
    off_mid: u16,
    off_hi: u32,
    _r: u32,
}

#[repr(C, align(16))]
struct Idt([Gate; 256]);

static mut IDT: Idt = Idt([Gate { off_lo: 0, sel: 0, ist: 0, attr: 0, off_mid: 0, off_hi: 0, _r: 0 }; 256]);

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

/// Double faults and NMIs run on their own stack (IST 1), so a kernel stack overflow still
/// produces a report instead of a triple fault.
pub const IST_DOUBLE_FAULT: u8 = 1;

pub fn init() {
    unsafe {
        let idt = &mut *core::ptr::addr_of_mut!(IDT);
        for (v, g) in idt.0.iter_mut().enumerate() {
            let a = trap::stub_address(v);
            *g = Gate {
                off_lo: a as u16,
                sel: gdt::KCODE,
                ist: if v == 8 || v == 2 { IST_DOUBLE_FAULT } else { 0 },
                // Present, interrupt gate; int3 (3) and the legacy syscall vector are callable
                // from ring 3.
                attr: if v == 3 { 0xee } else { 0x8e },
                off_mid: (a >> 16) as u16,
                off_hi: (a >> 32) as u32,
                _r: 0,
            };
        }
    }
    load();
}

pub fn load() {
    let p = Pointer { limit: (core::mem::size_of::<Idt>() - 1) as u16, base: core::ptr::addr_of!(IDT) as u64 };
    unsafe { asm!("lidt [{}]", in(reg) &p) };
}
