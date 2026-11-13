//! Per-CPU data, reached through the GS base in kernel mode.
//!
//! The first fields have fixed offsets used by assembly: 0 = this block's address, 8 = top of
//! the current task's kernel stack (syscall entry), 16 = scratch for the user rsp.
use alloc::boxed::Box;
use core::arch::asm;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::cpu;
use super::gdt::{Gdt, Tss};

pub const MAX_CPUS: usize = 64;

#[repr(C)]
pub struct PerCpu {
    self_ptr: u64,
    pub kernel_stack_top: u64,
    pub user_rsp: u64,
    /// The running task (raw pointer, offset 24): read with one gs-relative load, so it cannot
    /// be torn by a migration between reading the per-CPU base and the field.
    pub current_task: u64,
    pub id: usize,
    /// The CR3 this CPU has loaded, and the TLB generation of that address space at the time
    /// (see sched: kernel threads and the idle task keep the last user address space loaded).
    pub loaded_cr3: AtomicU64,
    pub loaded_gen: AtomicU64,
    pub apic_id: u32,
    pub gdt: Gdt,
    pub tss: Tss,
    /// Ticks of the local timer seen by this CPU.
    pub ticks: AtomicU64,
    /// Nesting depth of interrupt handlers on this CPU.
    pub irq_depth: AtomicUsize,
    /// Scheduler state for this CPU (see sched).
    pub sched: crate::sched::CpuSched,
}

static mut CPUS: [*mut PerCpu; MAX_CPUS] = [core::ptr::null_mut(); MAX_CPUS];
static ONLINE: AtomicUsize = AtomicUsize::new(0);

/// Creates and installs this CPU's block (GDT, TSS, GS base).
pub fn init_this_cpu(id: usize, apic_id: u32, double_fault_stack: u64) -> &'static mut PerCpu {
    let pc = Box::leak(Box::new(PerCpu {
        self_ptr: 0,
        kernel_stack_top: 0,
        user_rsp: 0,
        current_task: 0,
        id,
        loaded_cr3: AtomicU64::new(cpu::read_cr3() & crate::mm::paging::ADDR),
        loaded_gen: AtomicU64::new(0),
        apic_id,
        gdt: Gdt::new(),
        tss: Tss::default(),
        ticks: AtomicU64::new(0),
        irq_depth: AtomicUsize::new(0),
        sched: crate::sched::CpuSched::new(),
    }));
    pc.self_ptr = pc as *mut PerCpu as u64;
    pc.tss.ist[0] = double_fault_stack;
    pc.tss.iomap = core::mem::size_of::<Tss>() as u16;
    let tss: &'static Tss = unsafe { &*(&pc.tss as *const Tss) };
    let gdt: &'static mut Gdt = unsafe { &mut *(&mut pc.gdt as *mut Gdt) };
    gdt.load(tss);
    cpu::wrmsr(cpu::MSR_GS_BASE, pc.self_ptr);
    cpu::wrmsr(cpu::MSR_KERNEL_GS_BASE, 0);
    unsafe { CPUS[id] = pc };
    ONLINE.fetch_add(1, Ordering::SeqCst);
    pc
}

/// The current CPU's block.
#[inline]
pub fn this() -> &'static mut PerCpu {
    let p: u64;
    unsafe { asm!("mov {}, gs:[0]", out(reg) p, options(nostack, preserves_flags, readonly)) };
    unsafe { &mut *(p as *mut PerCpu) }
}

pub fn cpu_id() -> usize {
    this().id
}

pub fn online() -> usize {
    ONLINE.load(Ordering::SeqCst)
}

pub fn get(id: usize) -> Option<&'static mut PerCpu> {
    unsafe {
        let p = (*core::ptr::addr_of!(CPUS))[id];
        if p.is_null() { None } else { Some(&mut *p) }
    }
}

/// Sets the kernel stack used on the next entry from user mode (interrupts and syscall).
pub fn set_kernel_stack(top: u64) {
    let pc = this();
    pc.kernel_stack_top = top;
    pc.tss.rsp0 = top;
}
