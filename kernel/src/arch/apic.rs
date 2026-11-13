//! Local APIC (timer, IPIs, end of interrupt) and I/O APIC (device interrupts).
use core::sync::atomic::{AtomicU64, Ordering};

use super::{cpu, port};
use crate::mm::paging;
use crate::sync::{Once, SpinLock};

static LAPIC: Once<u64> = Once::new();
static IOAPIC: Once<u64> = Once::new();
static IOAPIC_LOCK: SpinLock<()> = SpinLock::new(());
/// LAPIC timer ticks per millisecond (calibrated against the PIT).
static TIMER_PER_MS: AtomicU64 = AtomicU64::new(0);
/// TSC cycles per microsecond.
static TSC_PER_US: AtomicU64 = AtomicU64::new(0);

pub const TIMER_VECTOR: u8 = 0x20;
pub const SPURIOUS_VECTOR: u8 = 0xff;
pub const IPI_RESCHEDULE: u8 = 0xf0;
pub const IPI_HALT: u8 = 0xf1;
pub const IPI_TLB: u8 = 0xf2;
/// Device interrupts (GSI n) arrive on vector IRQ_BASE + n.
pub const IRQ_BASE: u8 = 0x30;

const ID: u32 = 0x20;
const EOI: u32 = 0xb0;
const SVR: u32 = 0xf0;
const ICR_LO: u32 = 0x300;
const ICR_HI: u32 = 0x310;
const LVT_TIMER: u32 = 0x320;
const TIMER_INIT: u32 = 0x380;
const TIMER_CUR: u32 = 0x390;
const TIMER_DIV: u32 = 0x3e0;

/// x2APIC mode: registers are MSRs (0x800 + offset/16). Under KVM this matters a lot: an MSR
/// write to the ICR or EOI is handled on a fast path, while every xAPIC MMIO access is a full
/// exit to the hypervisor (a cross-CPU wake-up took three of them).
static X2APIC: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn x2() -> bool {
    X2APIC.load(Ordering::Relaxed)
}

fn lapic_read(reg: u32) -> u32 {
    if x2() {
        return cpu::rdmsr(0x800 + (reg >> 4)) as u32;
    }
    unsafe { core::ptr::read_volatile((LAPIC.get() + reg as u64) as *const u32) }
}
fn lapic_write(reg: u32, v: u32) {
    if x2() {
        cpu::wrmsr(0x800 + (reg >> 4), v as u64);
        return;
    }
    unsafe { core::ptr::write_volatile((LAPIC.get() + reg as u64) as *mut u32, v) }
}

pub fn map(lapic_phys: u64, ioapic_phys: u64) {
    LAPIC.set(paging::map_mmio(lapic_phys, 4096));
    let no_x2 = crate::BOOT_INFO.get().arg("x2apic") == Some("off");
    X2APIC.store(!no_x2 && cpu::cpuid(1, 0).ecx & (1 << 21) != 0, Ordering::Relaxed);
    IOAPIC.set(paging::map_mmio(ioapic_phys, 4096));
    // The legacy PICs: remap away from the exception vectors and mask everything.
    for (p, v) in [(0x20, 0x11), (0xa0, 0x11), (0x21, 0x20), (0xa1, 0x28), (0x21, 4), (0xa1, 2), (0x21, 1), (0xa1, 1), (0x21, 0xff), (0xa1, 0xff)] {
        port::outb(p, v);
    }
}

/// Enables this CPU's local APIC.
pub fn init_local() {
    let mut base = cpu::rdmsr(cpu::MSR_APIC_BASE) | (1 << 11);
    if x2() {
        base |= 1 << 10;
    }
    cpu::wrmsr(cpu::MSR_APIC_BASE, base);
    lapic_write(SVR, 0x100 | SPURIOUS_VECTOR as u32);
}

/// This CPU's APIC ID (valid before or after init_local).
pub fn id() -> u32 {
    if cpu::rdmsr(cpu::MSR_APIC_BASE) & (1 << 10) != 0 {
        return cpu::rdmsr(0x802) as u32;
    }
    unsafe { core::ptr::read_volatile((LAPIC.get() + ID as u64) as *const u32) >> 24 }
}

pub fn x2apic_enabled() -> bool {
    x2()
}

pub fn eoi() {
    lapic_write(EOI, 0);
}

/// Measures the LAPIC timer and the TSC against 10 ms of PIT channel 2 (done once, on the BSP).
pub fn calibrate() {
    lapic_write(TIMER_DIV, 0x3); // divide by 16
    // PIT channel 2, gated by port 0x61, one-shot for 10 ms.
    let ticks: u16 = 11932; // 1193182 Hz * 10 ms
    port::outb(0x61, (port::inb(0x61) & !0x02) | 0x01);
    port::outb(0x43, 0xb0);
    port::outb(0x42, ticks as u8);
    port::outb(0x42, (ticks >> 8) as u8);
    let g = port::inb(0x61);
    port::outb(0x61, g & !0x01);
    port::outb(0x61, g | 0x01);
    lapic_write(TIMER_INIT, u32::MAX);
    let t0 = cpu::rdtsc();
    while port::inb(0x61) & 0x20 == 0 {
        core::hint::spin_loop();
    }
    let t1 = cpu::rdtsc();
    let elapsed = u32::MAX - lapic_read(TIMER_CUR);
    lapic_write(TIMER_INIT, 0);
    TIMER_PER_MS.store(elapsed as u64 / 10, Ordering::Relaxed);
    TSC_PER_US.store((t1 - t0) / 10_000, Ordering::Relaxed);
}

/// Starts the periodic timer at `hz` on this CPU.
pub fn start_timer(hz: u64) {
    lapic_write(TIMER_DIV, 0x3);
    lapic_write(LVT_TIMER, TIMER_VECTOR as u32 | (1 << 17)); // periodic
    lapic_write(TIMER_INIT, (TIMER_PER_MS.load(Ordering::Relaxed) * 1000 / hz) as u32);
}

pub fn tsc_per_us() -> u64 {
    TSC_PER_US.load(Ordering::Relaxed).max(1)
}

/// Nanoseconds since boot (from the TSC).
pub fn now_ns() -> u64 {
    let t = cpu::rdtsc() as u128;
    (t * 1000 / tsc_per_us() as u128) as u64
}

fn send_ipi(apic_id: u32, low: u32) {
    if x2() {
        // One 64-bit write; no delivery-status bit to wait for.
        cpu::wrmsr(0x830, ((apic_id as u64) << 32) | low as u64);
        return;
    }
    while lapic_read(ICR_LO) & (1 << 12) != 0 {
        core::hint::spin_loop();
    }
    lapic_write(ICR_HI, apic_id << 24);
    lapic_write(ICR_LO, low);
}

pub fn ipi(apic_id: u32, vector: u8) {
    send_ipi(apic_id, vector as u32);
}

pub fn ipi_all_others(vector: u8) {
    send_ipi(0, vector as u32 | (3 << 18));
}

/// INIT, then two STARTUPs at the trampoline page (Intel's MP start-up sequence).
pub fn start_ap(apic_id: u32, trampoline_page: u8) {
    send_ipi(apic_id, 0x4500); // INIT, assert
    delay_us(10_000);
    for _ in 0..2 {
        send_ipi(apic_id, 0x4600 | trampoline_page as u32); // STARTUP
        delay_us(200);
    }
}

pub fn delay_us(us: u64) {
    let end = cpu::rdtsc() + us * tsc_per_us();
    while cpu::rdtsc() < end {
        core::hint::spin_loop();
    }
}

fn ioapic_write(reg: u32, v: u32) {
    let base = *IOAPIC.get();
    unsafe {
        core::ptr::write_volatile(base as *mut u32, reg);
        core::ptr::write_volatile((base + 0x10) as *mut u32, v);
    }
}

/// Routes GSI `gsi` to vector IRQ_BASE + gsi on the CPU with `apic_id`.
pub fn route_irq(gsi: u32, apic_id: u32, level_low: bool) {
    let _g = IOAPIC_LOCK.lock();
    let mut lo = IRQ_BASE as u32 + gsi;
    if level_low {
        lo |= (1 << 13) | (1 << 15); // active low, level triggered (PCI)
    }
    ioapic_write(0x10 + 2 * gsi, lo);
    ioapic_write(0x11 + 2 * gsi, apic_id << 24);
}

/// TSC frequency in MHz (from the calibration against the PIT).
pub fn tsc_mhz() -> u64 {
    tsc_per_us()
}
