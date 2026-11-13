//! Sampling profiler. While on, every CPU's local timer runs at RATE Hz instead of the
//! scheduler's HZ and each tick records where the CPU was: a kernel instruction address, a user
//! process, or idle. The scheduler still sees HZ ticks.
//!
//! Results: /proc/profile (whole histogram), and with streaming on, a summary line every 500 ms
//! on COM2 that the web console (web/) turns into a live view:
//!   P <ms> <samples> <idle> <user> k:<rip hex>:<count> ... u:<pid>:<name>:<count> ...
//! Control: syscall 1000 (cmd 1 = start, 2 = start and stream, 0 = stop) or profile=1 / 2 on the
//! kernel command line.
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::arch::{apic, percpu, trap::TrapFrame};
use crate::sync::SpinLock;
use kcore::errno::*;

pub const RATE: u64 = 1000;
static ENABLED: AtomicBool = AtomicBool::new(false);
static STREAM: AtomicBool = AtomicBool::new(false);
static SUB: [AtomicU32; percpu::MAX_CPUS] = [const { AtomicU32::new(0) }; percpu::MAX_CPUS];

struct Hist {
    kernel: BTreeMap<u64, u32>,
    user: BTreeMap<u32, u32>,
    idle: u64,
    total: u64,
}

impl Hist {
    const fn new() -> Hist {
        Hist { kernel: BTreeMap::new(), user: BTreeMap::new(), idle: 0, total: 0 }
    }
}

static TOTAL: SpinLock<Hist> = SpinLock::new(Hist::new());
static WINDOW: SpinLock<Hist> = SpinLock::new(Hist::new());
static STARTED_AT: AtomicU64 = AtomicU64::new(0);

pub fn timer_hz() -> u64 {
    if ENABLED.load(Ordering::Relaxed) { RATE } else { crate::sched::HZ }
}

fn record(h: &mut Hist, f: &TrapFrame, idle: bool, pid: u32) {
    h.total += 1;
    if idle {
        h.idle += 1;
    } else if f.from_user() {
        *h.user.entry(pid).or_insert(0) += 1;
    } else {
        *h.kernel.entry(f.rip).or_insert(0) += 1;
    }
}

/// The timer interrupt: takes a sample when profiling; returns whether the scheduler's tick is
/// due (every RATE/HZ samples while profiling, always otherwise).
pub fn on_timer(f: &TrapFrame) -> bool {
    if !ENABLED.load(Ordering::Relaxed) {
        return true;
    }
    let idle = crate::sched::current_ref().is_idle && !f.from_user();
    let pid = if f.from_user() { crate::proc::try_current().map(|p| p.pid).unwrap_or(0) } else { 0 };
    record(&mut TOTAL.lock(), f, idle, pid);
    if STREAM.load(Ordering::Relaxed) {
        record(&mut WINDOW.lock(), f, idle, pid);
    }
    let c = &SUB[percpu::cpu_id()];
    let n = c.fetch_add(1, Ordering::Relaxed) + 1;
    if n as u64 >= RATE / crate::sched::HZ {
        c.store(0, Ordering::Relaxed);
        true
    } else {
        false
    }
}

fn set_rate_everywhere() {
    apic::start_timer(timer_hz());
    for i in 0..percpu::online() {
        if let Some(pc) = percpu::get(i) {
            if i != percpu::cpu_id() {
                apic::ipi(pc.apic_id, apic::IPI_PROFILE);
            }
        }
    }
}

pub fn start(stream: bool) {
    *TOTAL.lock() = Hist::new();
    *WINDOW.lock() = Hist::new();
    STARTED_AT.store(apic::now_ns(), Ordering::Relaxed);
    STREAM.store(stream, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Release);
    set_rate_everywhere();
    static THREAD: AtomicBool = AtomicBool::new(false);
    if stream && !THREAD.swap(true, Ordering::AcqRel) {
        crate::serial::init_port(crate::serial::COM2);
        crate::sched::spawn_kernel("profiler", streamer);
    }
}

pub fn stop() {
    ENABLED.store(false, Ordering::Release);
    STREAM.store(false, Ordering::Relaxed);
    set_rate_everywhere();
}

pub fn control(cmd: u64, _arg: u64) -> KResult<u64> {
    match cmd {
        0 => stop(),
        1 => start(false),
        2 => start(true),
        _ => return Err(EINVAL),
    }
    Ok(0)
}

/// IPI_PROFILE: another CPU changed the sampling state.
pub fn on_ipi() {
    apic::start_timer(timer_hz());
}

fn top<K: Copy + Ord>(m: &BTreeMap<K, u32>, n: usize) -> Vec<(K, u32)> {
    let mut v: Vec<(K, u32)> = m.iter().map(|(k, c)| (*k, *c)).collect();
    v.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    v.truncate(n);
    v
}

fn proc_name(pid: u32) -> String {
    crate::proc::by_pid(pid).map(|p| p.name.lock().clone()).unwrap_or_else(|| String::from("?"))
}

/// The streaming thread: one summary line on COM2 every 500 ms.
fn streamer() {
    loop {
        crate::sched::sleep_ns(500_000_000);
        if !STREAM.load(Ordering::Relaxed) {
            continue;
        }
        let h = core::mem::replace(&mut *WINDOW.lock(), Hist::new());
        let mut s = String::new();
        let ms = (apic::now_ns() - STARTED_AT.load(Ordering::Relaxed)) / 1_000_000;
        let user_total: u64 = h.user.values().map(|&c| c as u64).sum();
        let _ = write!(s, "P {ms} {} {} {}", h.total, h.idle, user_total);
        for (rip, c) in top(&h.kernel, 40) {
            let _ = write!(s, " k:{rip:x}:{c}");
        }
        for (pid, c) in top(&h.user, 16) {
            let _ = write!(s, " u:{pid}:{}:{c}", proc_name(pid).replace(' ', "_"));
        }
        s.push('\n');
        for b in s.bytes() {
            crate::serial::putb(crate::serial::COM2, b);
        }
    }
}

/// /proc/profile.
pub fn report() -> String {
    let h = TOTAL.lock();
    let mut s = String::new();
    let _ = writeln!(s, "samples {} idle {} running {}", h.total, h.idle, if ENABLED.load(Ordering::Relaxed) { "yes" } else { "no" });
    for (pid, c) in top(&h.user, 64) {
        let _ = writeln!(s, "user {c} {pid} {}", proc_name(pid));
    }
    for (rip, c) in top(&h.kernel, 4096) {
        let _ = writeln!(s, "kernel {c} {rip:#x}");
    }
    s
}
