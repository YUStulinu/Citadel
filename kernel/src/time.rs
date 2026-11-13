//! Time: the wall clock (read from the CMOS RTC once at boot) and the monotonic clock (TSC).
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::{apic, port};

static BOOT_REALTIME_NS: AtomicU64 = AtomicU64::new(0);
static BOOT_MONO_NS: AtomicU64 = AtomicU64::new(0);

fn cmos(reg: u8) -> u8 {
    port::outb(0x70, reg);
    port::inb(0x71)
}

fn bcd(v: u8) -> u64 {
    ((v >> 4) * 10 + (v & 15)) as u64
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u64, d: u64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as i64;
    era * 146097 + doe - 719468
}

pub fn init() {
    // Wait for an update to finish, then read (values are BCD unless status B says binary).
    while cmos(0x0a) & 0x80 != 0 {}
    let b = cmos(0x0b);
    let conv = |v: u8| if b & 4 != 0 { v as u64 } else { bcd(v) };
    let (s, mi, h, d, mo, y) = (conv(cmos(0)), conv(cmos(2)), conv(cmos(4) & 0x7f), conv(cmos(7)), conv(cmos(8)), conv(cmos(9)));
    let days = days_from_civil(2000 + y as i64, mo, d);
    let secs = days as u64 * 86400 + h * 3600 + mi * 60 + s;
    BOOT_REALTIME_NS.store(secs * 1_000_000_000, Ordering::Relaxed);
    BOOT_MONO_NS.store(apic::now_ns(), Ordering::Relaxed);
}

/// Nanoseconds since boot.
pub fn monotonic_ns() -> u64 {
    apic::now_ns() - BOOT_MONO_NS.load(Ordering::Relaxed)
}

/// Nanoseconds since the Unix epoch.
pub fn realtime_ns() -> u64 {
    BOOT_REALTIME_NS.load(Ordering::Relaxed) + monotonic_ns()
}
