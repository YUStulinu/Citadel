//! Starting the other CPUs (application processors).
//!
//! An AP starts in 16-bit real mode at a page below 1 MiB given in the STARTUP IPI. The
//! trampoline (copied to physical 0x8000) moves it to protected mode, enables PAE and long mode
//! with a temporary page table (the kernel's upper half plus an identity map of the first 2 MiB,
//! where the trampoline runs), and jumps to `ap_main` on its own stack.
use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{apic, cpu, percpu};
use crate::boot::phys_to_virt;
use crate::mm::{frame, paging};

const TRAMPOLINE: u64 = 0x8000;

global_asm!(
    r#"
    .section .text
    .global ap_trampoline_start
    .global ap_trampoline_end
    .global ap_tr_cr3
    .global ap_tr_stack
    .global ap_tr_entry
    .global ap_tr_arg
    .code16
ap_trampoline_start:
    cli
    cld
    xorw %ax, %ax
    movw %ax, %ds
    lgdtl (ap_tr_gdtr - ap_trampoline_start + 0x8000)
    movl %cr0, %eax
    orl $1, %eax
    movl %eax, %cr0
    ljmpl $0x08, $(ap_tr_pm32 - ap_trampoline_start + 0x8000)
    .code32
ap_tr_pm32:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movl %cr4, %eax
    orl $0x20, %eax
    movl %eax, %cr4
    movl (ap_tr_cr3 - ap_trampoline_start + 0x8000), %eax
    movl %eax, %cr3
    movl $0xC0000080, %ecx
    rdmsr
    orl $0x900, %eax
    wrmsr
    movl %cr0, %eax
    orl $0x80000001, %eax
    movl %eax, %cr0
    ljmpl $0x18, $(ap_tr_lm64 - ap_trampoline_start + 0x8000)
    .code64
ap_tr_lm64:
    xorl %eax, %eax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movq (ap_tr_stack - ap_trampoline_start + 0x8000), %rsp
    movq (ap_tr_arg - ap_trampoline_start + 0x8000), %rdi
    movq (ap_tr_entry - ap_trampoline_start + 0x8000), %rax
    jmpq *%rax
    .balign 8
ap_tr_gdt:
    .quad 0
    .quad 0x00cf9a000000ffff
    .quad 0x00cf92000000ffff
    .quad 0x00af9a000000ffff
ap_tr_gdtr:
    .word 31
    .long ap_tr_gdt - ap_trampoline_start + 0x8000
    .balign 8
ap_tr_cr3: .quad 0
ap_tr_stack: .quad 0
ap_tr_entry: .quad 0
ap_tr_arg: .quad 0
ap_trampoline_end:
"#,
    options(att_syntax)
);

unsafe extern "C" {
    static ap_trampoline_start: u8;
    static ap_trampoline_end: u8;
    static ap_tr_cr3: u8;
    static ap_tr_stack: u8;
    static ap_tr_entry: u8;
    static ap_tr_arg: u8;
}

static AP_READY: AtomicBool = AtomicBool::new(false);

fn field(sym: &u8) -> *mut u64 {
    let off = sym as *const u8 as u64 - unsafe { &ap_trampoline_start as *const u8 as u64 };
    phys_to_virt(TRAMPOLINE + off) as *mut u64
}

/// Starts every CPU in `apic_ids` other than this one; returns how many came up.
pub fn start_aps(apic_ids: &[u32]) -> usize {
    let me = apic::id();
    // Copy the trampoline.
    unsafe {
        let start = &ap_trampoline_start as *const u8;
        let len = &ap_trampoline_end as *const u8 as usize - start as usize;
        core::ptr::copy_nonoverlapping(start, phys_to_virt(TRAMPOLINE) as *mut u8, len);
    }
    // A temporary PML4 below 4 GiB: the kernel half plus identity for the first 2 MiB.
    let pml4 = loop {
        let p = frame::alloc_zeroed(0).expect("no memory");
        if p < (1 << 32) {
            break p;
        }
    };
    let k = unsafe { &*(phys_to_virt(paging::kernel_pml4()) as *const [u64; 512]) };
    let t = unsafe { &mut *(phys_to_virt(pml4) as *mut [u64; 512]) };
    t[256..].copy_from_slice(&k[256..]);
    let pdpt = frame::alloc_zeroed(0).unwrap();
    let pd = frame::alloc_zeroed(0).unwrap();
    t[0] = pdpt | 3;
    unsafe {
        (*(phys_to_virt(pdpt) as *mut [u64; 512]))[0] = pd | 3;
        (*(phys_to_virt(pd) as *mut [u64; 512]))[0] = 0x83; // 2 MiB page at 0
    }
    let mut started = 0;
    for (i, &id) in apic_ids.iter().filter(|&&id| id != me).enumerate() {
        let stack = frame::alloc_zeroed(2).expect("no memory for an AP stack");
        unsafe {
            *field(&ap_tr_cr3) = pml4;
            *field(&ap_tr_stack) = phys_to_virt(stack) + 16384;
            *field(&ap_tr_entry) = ap_main as *const () as usize as u64;
            *field(&ap_tr_arg) = (i + 1) as u64;
        }
        AP_READY.store(false, Ordering::SeqCst);
        apic::start_ap(id, (TRAMPOLINE >> 12) as u8);
        let deadline = cpu::rdtsc() + 200_000 * apic::tsc_per_us();
        while !AP_READY.load(Ordering::SeqCst) && cpu::rdtsc() < deadline {
            core::hint::spin_loop();
        }
        if AP_READY.load(Ordering::SeqCst) {
            started += 1;
        } else {
            crate::kprintln!("smp: CPU with APIC id {id} did not start");
        }
    }
    started
}

extern "C" fn ap_main(index: u64) -> ! {
    cpu::write_cr3(paging::kernel_pml4());
    cpu::init_features();
    let df = frame::alloc_zeroed(1).expect("no memory");
    percpu::init_this_cpu(index as usize, apic::id(), phys_to_virt(df) + 8192);
    super::idt::load();
    apic::init_local();
    crate::syscall::init_msrs();
    crate::sched::init_cpu();
    apic::start_timer(crate::sched::HZ);
    AP_READY.store(true, Ordering::SeqCst);
    crate::sched::idle_loop();
}
