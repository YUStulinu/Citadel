//! Routing of interrupts and exceptions.
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use crate::arch::{apic, cpu, percpu, trap::TrapFrame};
use crate::sync::SpinLock;

type Handler = Box<dyn Fn() + Send + Sync>;

static HANDLERS: SpinLock<Vec<(u8, Handler)>> = SpinLock::new(Vec::new());

/// Registers a handler for device interrupt vector `vector`.
pub fn register(vector: u8, h: impl Fn() + Send + Sync + 'static) {
    HANDLERS.lock().push((vector, Box::new(h)));
}

const NAMES: [&str; 21] = [
    "divide error", "debug", "NMI", "breakpoint", "overflow", "bound range", "invalid opcode", "device not available",
    "double fault", "coprocessor overrun", "invalid TSS", "segment not present", "stack fault", "general protection",
    "page fault", "reserved", "x87 floating point", "alignment check", "machine check", "SIMD floating point", "virtualization",
];

pub fn handle(f: &mut TrapFrame) {
    let v = f.vector as u8;
    let pc = percpu::this();
    // Per-CPU counter: a plain load and store, no locked instruction.
    pc.irq_depth.store(pc.irq_depth.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    match v {
        14 => crate::mm::fault::page_fault(f, cpu::read_cr2()),
        0..=31 => exception(f),
        apic::TIMER_VECTOR => {
            apic::eoi();
            crate::profile::sample(f);
            crate::sched::tick();
        }
        apic::IPI_RESCHEDULE => {
            apic::eoi();
            pc.sched.need_resched.store(true, Ordering::Relaxed);
        }
        apic::IPI_TLB => {
            apic::eoi();
            crate::sched::handle_tlb_ipi();
        }
        apic::IPI_HALT => {
            apic::eoi();
            cpu::halt_forever();
        }
        apic::SPURIOUS_VECTOR => {}
        _ => {
            {
                let hs = HANDLERS.lock();
                for (hv, h) in hs.iter() {
                    if *hv == v {
                        h();
                    }
                }
            }
            apic::eoi();
        }
    }
    pc.irq_depth.store(pc.irq_depth.load(Ordering::Relaxed) - 1, Ordering::Relaxed);
    // Preemption point: on the way back to user mode, or out of the idle loop.
    if pc.sched.need_resched.load(Ordering::Relaxed) && (f.from_user() || crate::sched::try_current().is_some_and(|t| t.is_idle)) {
        crate::sched::schedule();
    }
    if f.from_user() {
        crate::signal::deliver_pending(f);
    }
}

fn exception(f: &mut TrapFrame) {
    let name = NAMES.get(f.vector as usize).copied().unwrap_or("exception");
    if f.from_user() {
        // A fault in a user program: the matching signal (SIGFPE, SIGILL, SIGSEGV, SIGTRAP).
        let sig = match f.vector {
            0 | 16 | 19 => crate::signal::SIGFPE,
            6 => crate::signal::SIGILL,
            3 | 1 => crate::signal::SIGTRAP,
            _ => crate::signal::SIGSEGV,
        };
        crate::signal::force(sig, f);
        return;
    }
    panic!("{} in the kernel at {:#x} (error {:#x}, rsp {:#x})\n{:#x?}", name, f.rip, f.error, f.rsp, f);
}
