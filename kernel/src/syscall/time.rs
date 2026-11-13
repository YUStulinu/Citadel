//! Clocks and sleeping.
use kcore::errno::*;

use crate::user;

fn now(clock: u32) -> KResult<u64> {
    match clock {
        0 | 5 | 8 | 11 => Ok(crate::time::realtime_ns()), // REALTIME (+ COARSE, ALARM, TAI)
        1 | 4 | 6 | 7 | 9 => Ok(crate::time::monotonic_ns()), // MONOTONIC, RAW, COARSE, BOOTTIME
        2 | 3 => Ok(crate::sched::current_runtime_ns()), // CPU time
        _ => Err(EINVAL),
    }
}

pub fn clock_gettime(clock: u32, ts: u64) -> KResult<u64> {
    let ns = now(clock)?;
    user::write_struct(ts, &[ns / 1_000_000_000, ns % 1_000_000_000])?;
    Ok(0)
}

pub fn clock_getres(clock: u32, ts: u64) -> KResult<u64> {
    now(clock)?; // EINVAL for an unknown clock
    if ts != 0 {
        user::write_struct(ts, &[0u64, 1])?;
    }
    Ok(0)
}

pub fn gettimeofday(tv: u64) -> KResult<u64> {
    if tv != 0 {
        let ns = crate::time::realtime_ns();
        user::write_struct(tv, &[ns / 1_000_000_000, (ns % 1_000_000_000) / 1000])?;
    }
    Ok(0)
}

pub fn time(t: u64) -> KResult<u64> {
    let s = crate::time::realtime_ns() / 1_000_000_000;
    if t != 0 {
        user::put_u64(t, s)?;
    }
    Ok(s)
}

fn sleep(ns: u64, rem: u64) -> KResult<u64> {
    let start = crate::time::monotonic_ns();
    // A stop (SIGSTOP and SIGCONT) or an ignored signal does not end the sleep.
    loop {
        let left = ns.saturating_sub(crate::time::monotonic_ns() - start);
        if left == 0 || crate::sched::sleep_ns(left) {
            return Ok(0);
        }
        if crate::signal::must_interrupt() {
            break;
        }
    }
    if rem != 0 {
        let left = ns.saturating_sub(crate::time::monotonic_ns() - start);
        user::write_struct(rem, &[left / 1_000_000_000, left % 1_000_000_000])?;
    }
    Err(EINTR)
}

fn read_timespec(req: u64) -> KResult<(u64, u64)> {
    let [s, ns]: [i64; 2] = user::read_struct(req)?;
    if s < 0 || !(0..1_000_000_000).contains(&ns) {
        return Err(EINVAL);
    }
    Ok((s as u64, ns as u64))
}

pub fn nanosleep(req: u64, rem: u64) -> KResult<u64> {
    let (s, ns) = read_timespec(req)?;
    sleep(s * 1_000_000_000 + ns, rem)
}

pub fn clock_nanosleep(clock: u32, flags: u32, req: u64, rem: u64) -> KResult<u64> {
    if clock == 3 {
        return Err(EINVAL); // the thread's own CPU clock cannot advance while it sleeps
    }
    now(clock)?;
    let (s, ns) = read_timespec(req)?;
    let mut d = s * 1_000_000_000 + ns;
    if flags & 1 != 0 {
        // TIMER_ABSTIME: no remaining time is reported
        d = d.saturating_sub(now(clock)?);
        return sleep(d, 0);
    }
    sleep(d, rem)
}

pub fn times(buf: u64) -> KResult<u64> {
    let t = crate::sched::current_runtime_ns() / 10_000_000;
    if buf != 0 {
        user::write_struct(buf, &[t, 0u64, 0, 0])?;
    }
    Ok(crate::time::monotonic_ns() / 10_000_000)
}
