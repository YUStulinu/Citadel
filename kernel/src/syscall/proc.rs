//! Process and signal system calls.
use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use kcore::errno::*;

use crate::arch::trap::TrapFrame;
use crate::proc;
use crate::signal::{self, SigAction, bit};
use crate::user;

pub fn execve(f: &mut TrapFrame, path: u64, argv: u64, envp: u64) -> KResult<u64> {
    let path = user::read_path(path)?;
    let argv = user::read_str_array(argv, 128 * 1024)?;
    let envp = user::read_str_array(envp, 128 * 1024)?;
    let p = proc::current();
    crate::exec::exec(&p, &path, &argv, &envp, f)?;
    Ok(0)
}

/// clone() as used for fork (no CLONE_VM): threads are not supported.
pub fn clone(f: &mut TrapFrame, flags: u64) -> KResult<u64> {
    const CLONE_VM: u64 = 0x100;
    if flags & CLONE_VM != 0 {
        return Err(ENOSYS);
    }
    proc::fork(f).map(|p| p as u64)
}

pub fn wait4(pid: i64, status: u64, options: u64, _rusage: u64) -> KResult<u64> {
    let (child, ws) = proc::wait(pid, options)?;
    if status != 0 && child != 0 {
        user::put_i32(status, ws)?;
    }
    Ok(child as u64)
}

pub fn uname(buf: u64) -> KResult<u64> {
    let mut u = [0u8; 65 * 6];
    for (i, s) in ["Citadel", "citadel", "0.1.0", "#1 SMP", "x86_64", "(none)"].iter().enumerate() {
        u[65 * i..65 * i + s.len()].copy_from_slice(s.as_bytes());
    }
    user::copy_out(buf, &u)?;
    Ok(0)
}

pub fn getrlimit(res: u32, buf: u64) -> KResult<u64> {
    if buf == 0 {
        return Ok(0);
    }
    let (cur, max) = match res {
        3 => (8 << 20, u64::MAX),             // RLIMIT_STACK
        7 => (proc::MAX_FDS as u64, proc::MAX_FDS as u64), // RLIMIT_NOFILE
        _ => (u64::MAX, u64::MAX),
    };
    user::write_struct(buf, &[cur, max])?;
    Ok(0)
}

pub fn getrusage(buf: u64) -> KResult<u64> {
    let ns = crate::sched::current().runtime_ns.load(Ordering::Relaxed);
    let mut r = [0u64; 18];
    r[0] = ns / 1_000_000_000;
    r[1] = (ns % 1_000_000_000) / 1000;
    user::write_struct(buf, &r)?;
    Ok(0)
}

pub fn sysinfo(buf: u64) -> KResult<u64> {
    let mut s = [0u64; 14];
    s[0] = crate::time::monotonic_ns() / 1_000_000_000;
    s[4] = crate::mm::frame::total_bytes();
    s[5] = crate::mm::frame::free_bytes();
    s[10] = proc::all().len() as u64; // procs (u16 in Linux, packed with padding; good enough)
    s[13] = 1; // mem_unit
    user::write_struct(buf, &s)?;
    Ok(0)
}

pub fn setpgid(pid: u32, pgid: u32) -> KResult<u64> {
    let me = proc::current();
    let target = if pid == 0 { me.clone() } else { proc::by_pid(pid).ok_or(ESRCH)? };
    if target.pid != me.pid && target.ppid.load(Ordering::Relaxed) != me.pid {
        return Err(ESRCH);
    }
    let g = if pgid == 0 { target.pid } else { pgid };
    target.pgid.store(g, Ordering::Release);
    Ok(0)
}

pub fn setsid() -> KResult<u64> {
    let me = proc::current();
    if me.pgid.load(Ordering::Relaxed) == me.pid && me.pid != 1 {
        return Err(EPERM);
    }
    me.sid.store(me.pid, Ordering::Release);
    me.pgid.store(me.pid, Ordering::Release);
    Ok(me.pid as u64)
}

pub fn getpgid(pid: u32) -> KResult<u64> {
    let p = if pid == 0 { proc::current() } else { proc::by_pid(pid).ok_or(ESRCH)? };
    Ok(p.pgid.load(Ordering::Relaxed) as u64)
}

pub fn getsid(pid: u32) -> KResult<u64> {
    let p = if pid == 0 { proc::current() } else { proc::by_pid(pid).ok_or(ESRCH)? };
    Ok(p.sid.load(Ordering::Relaxed) as u64)
}

pub fn arch_prctl(code: u64, addr: u64) -> KResult<u64> {
    const ARCH_SET_FS: u64 = 0x1002;
    const ARCH_GET_FS: u64 = 0x1003;
    match code {
        ARCH_SET_FS => {
            if addr >= kcore::elf::USER_TOP {
                return Err(EPERM);
            }
            crate::arch::cpu::wrmsr(crate::arch::cpu::MSR_FS_BASE, addr);
            crate::sched::current().fs_base.store(addr, Ordering::Relaxed);
            Ok(0)
        }
        ARCH_GET_FS => {
            user::put_u64(addr, crate::arch::cpu::rdmsr(crate::arch::cpu::MSR_FS_BASE))?;
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}

/// futex: single-threaded processes only need WAKE to succeed and WAIT to not block forever.
pub fn futex(addr: u64, op: u32, val: u32) -> KResult<u64> {
    match op & 0x7f {
        0 => {
            if user::get_u32(addr)? != val {
                return Err(EAGAIN);
            }
            Err(EINTR)
        }
        1 => Ok(0),
        _ => Err(ENOSYS),
    }
}

pub fn getrandom(buf: u64, len: usize) -> KResult<u64> {
    let mut v = alloc::vec![0u8; len.min(1 << 20)];
    crate::fs::devfs::fill_random(&mut v);
    user::copy_out(buf, &v)?;
    Ok(v.len() as u64)
}

pub fn rt_sigaction(sig: u32, act: u64, old: u64) -> KResult<u64> {
    if sig == 0 || sig as usize >= signal::NSIG {
        return Err(EINVAL);
    }
    let p = proc::current();
    let new: Option<SigAction> = if act != 0 { Some(user::read_struct(act)?) } else { None };
    if new.is_some() && (sig == signal::SIGKILL || sig == signal::SIGSTOP) {
        return Err(EINVAL);
    }
    let prev = {
        let mut s = p.sig.lock();
        let prev = s.actions[sig as usize];
        if let Some(a) = new {
            s.actions[sig as usize] = a;
            if a.handler == signal::SIG_IGN {
                s.pending &= !bit(sig);
            }
        }
        prev
    };
    if old != 0 {
        user::write_struct(old, &prev)?;
    }
    Ok(0)
}

pub fn rt_sigprocmask(how: u32, set: u64, old: u64) -> KResult<u64> {
    let p = proc::current();
    let new = if set != 0 { Some(user::get_u64(set)?) } else { None };
    let prev = {
        let mut s = p.sig.lock();
        let prev = s.blocked;
        if let Some(n) = new {
            let n = n & !(bit(signal::SIGKILL) | bit(signal::SIGSTOP));
            s.blocked = match how {
                0 => prev | n,
                1 => prev & !n,
                2 => n,
                _ => return Err(EINVAL),
            };
        }
        prev
    };
    if old != 0 {
        user::put_u64(old, prev)?;
    }
    Ok(0)
}

pub fn rt_sigpending(set: u64) -> KResult<u64> {
    let p = proc::current();
    let pend = {
        let s = p.sig.lock();
        s.pending & s.blocked
    };
    user::put_u64(set, pend)?;
    Ok(0)
}

/// Waits for a signal with a temporary mask; always returns EINTR (after the handler runs).
pub fn rt_sigsuspend(mask: u64) -> KResult<u64> {
    let m = user::get_u64(mask)?;
    let p = proc::current();
    let old = {
        let mut s = p.sig.lock();
        let o = s.blocked;
        s.blocked = m & !(bit(signal::SIGKILL) | bit(signal::SIGSTOP));
        o
    };
    let me = crate::sched::current();
    let wq = crate::sched::WaitQueue::new();
    let pc: Arc<proc::Process> = p.clone();
    wq.wait_until(|| {
        let s = pc.sig.lock();
        s.pending & !s.blocked != 0
    });
    drop(me);
    // The handler runs with the temporary mask; its sigreturn restores the old one.
    p.sig.lock().restore_mask = Some(old);
    Err(EINTR)
}
