//! POSIX signals.
//!
//! A signal sent to a process is recorded as pending; if the process is asleep in an
//! interruptible wait, the wait returns EINTR. Pending, unblocked signals are delivered on the
//! way back to user mode: the default action (terminate, ignore), or a handler. For a handler
//! the kernel pushes a frame on the user stack holding the interrupted registers and signal
//! mask, and sets things up so that the handler returns into `sa_restorer`, which calls
//! rt_sigreturn to restore them.
use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use kcore::errno::*;

use crate::arch::{gdt, trap::TrapFrame};
use crate::proc::{self, Process};

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGILL: u32 = 4;
pub const SIGTRAP: u32 = 5;
pub const SIGABRT: u32 = 6;
pub const SIGBUS: u32 = 7;
pub const SIGFPE: u32 = 8;
pub const SIGKILL: u32 = 9;
pub const SIGUSR1: u32 = 10;
pub const SIGSEGV: u32 = 11;
pub const SIGUSR2: u32 = 12;
pub const SIGPIPE: u32 = 13;
pub const SIGALRM: u32 = 14;
pub const SIGTERM: u32 = 15;
pub const SIGCHLD: u32 = 17;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGTSTP: u32 = 20;
pub const SIGTTIN: u32 = 21;
pub const SIGTTOU: u32 = 22;
pub const SIGURG: u32 = 23;
pub const SIGWINCH: u32 = 28;
pub const NSIG: usize = 65;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;
pub const SA_NOCLDSTOP: u64 = 1;
pub const SA_NOCLDWAIT: u64 = 2;
pub const SA_ONSTACK: u64 = 0x0800_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_SIGINFO: u64 = 4;

/// The kernel's struct sigaction (rt_sigaction layout on x86-64).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

#[derive(Clone)]
pub struct SigState {
    pub actions: [SigAction; NSIG],
    pub pending: u64,
    pub blocked: u64,
    /// sigsuspend: the mask to restore when the handler returns.
    pub restore_mask: Option<u64>,
    /// sigaltstack: (base, size) of the alternate signal stack.
    pub alt_stack: Option<(u64, u64)>,
    /// What the handler's siginfo_t says about each pending signal: (si_code, si_pid,
    /// si_uid, si_status).
    pub info: [(i32, u32, u32, i32); NSIG],
}

impl Default for SigState {
    fn default() -> Self {
        SigState { actions: [SigAction::default(); NSIG], pending: 0, blocked: 0, restore_mask: None, alt_stack: None, info: [(0, 0, 0, 0); NSIG] }
    }
}

impl SigState {
    pub fn inherit_from(&mut self, parent: &SigState) {
        self.actions = parent.actions;
        self.blocked = parent.blocked;
        self.pending = 0;
        self.alt_stack = parent.alt_stack;
    }
    /// exec: handlers go back to the default; ignored signals stay ignored.
    pub fn reset_on_exec(&mut self) {
        self.alt_stack = None;
        for a in self.actions.iter_mut() {
            if a.handler != SIG_IGN {
                *a = SigAction::default();
            }
        }
    }
    fn deliverable(&self) -> u64 {
        let unblockable = bit(SIGKILL) | bit(SIGSTOP);
        self.pending & (!self.blocked | unblockable)
    }
}

pub fn bit(sig: u32) -> u64 {
    1u64 << (sig - 1)
}

fn default_ignored(sig: u32) -> bool {
    matches!(sig, SIGCHLD | SIGCONT | SIGURG | SIGWINCH)
}

fn is_stop(sig: u32) -> bool {
    matches!(sig, SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU)
}

/// The process ignores SIGCHLD or asked for SA_NOCLDWAIT: its children leave no zombies.
pub fn no_zombies(p: &Arc<Process>) -> bool {
    let a = p.sig.lock().actions[SIGCHLD as usize];
    a.handler == SIG_IGN || a.flags & SA_NOCLDWAIT != 0
}

/// Tells the parent that `p` stopped or continued (SIGCHLD unless it asked for SA_NOCLDSTOP).
fn notify_parent_job(p: &Arc<Process>, code: i32, status: i32) {
    if let Some(parent) = proc::by_pid(p.ppid.load(Ordering::Acquire)) {
        let quiet = parent.sig.lock().actions[SIGCHLD as usize].flags & SA_NOCLDSTOP != 0;
        if !quiet {
            send_info(&parent, SIGCHLD, (code, p.pid, p.uid.load(Ordering::Relaxed), status));
        }
        parent.child_wq.wake_all();
    }
}

/// Stops the current process until SIGCONT (or SIGKILL) arrives.
fn stop_current(p: &Arc<Process>, sig: u32) {
    p.stopped.store(true, Ordering::Release);
    p.cont_report.store(false, Ordering::Release);
    p.stop_report.store(((sig << 8) | 0x7f) as i32, Ordering::Release);
    notify_parent_job(p, CLD_STOPPED, sig as i32);
    let killed = || p.sig.lock().pending & bit(SIGKILL) != 0;
    while p.stopped.load(Ordering::Acquire) && !killed() {
        crate::sched::current_ref().interrupted.store(false, Ordering::Release);
        p.stop_wq.wait_until(|| !p.stopped.load(Ordering::Acquire) || killed());
    }
}

fn dumps_core(sig: u32) -> bool {
    matches!(sig, SIGQUIT | SIGILL | SIGTRAP | SIGABRT | SIGBUS | SIGFPE | SIGSEGV)
}

pub const CLD_EXITED: i32 = 1;
pub const CLD_KILLED: i32 = 2;
pub const CLD_DUMPED: i32 = 3;
pub const CLD_STOPPED: i32 = 5;
pub const CLD_CONTINUED: i32 = 6;

/// Sends `sig` from the kernel (si_code SI_KERNEL-like 0, no sender).
pub fn send(p: &Arc<Process>, sig: u32) {
    send_info(p, sig, (0, 0, 0, 0));
}

/// Sends `sig` with what siginfo_t will report: (si_code, si_pid, si_uid, si_status).
pub fn send_info(p: &Arc<Process>, sig: u32, info: (i32, u32, u32, i32)) {
    if sig == 0 || sig as usize >= NSIG || p.state.load(Ordering::Acquire) != proc::ALIVE {
        return;
    }
    let mut continued = false;
    let wake = {
        let mut s = p.sig.lock();
        // Stop and continue signals cancel each other's pending instances.
        if sig == SIGCONT {
            s.pending &= !(bit(SIGSTOP) | bit(SIGTSTP) | bit(SIGTTIN) | bit(SIGTTOU));
            continued = p.stopped.swap(false, Ordering::AcqRel);
        } else if is_stop(sig) {
            s.pending &= !bit(SIGCONT);
        }
        let a = s.actions[sig as usize];
        let blocked = s.blocked & bit(sig) != 0;
        // An ignored signal is discarded right away; one that is ignored by default stays
        // pending while it is blocked (sigpending must show it).
        if sig != SIGKILL && (a.handler == SIG_IGN || (a.handler == SIG_DFL && default_ignored(sig) && !blocked)) {
            false
        } else {
            s.pending |= bit(sig);
            s.info[sig as usize] = info;
            s.deliverable() != 0
        }
    };
    if continued {
        p.cont_report.store(true, Ordering::Release);
        p.stop_report.store(0, Ordering::Release);
        p.stop_wq.wake_all();
        notify_parent_job(p, CLD_CONTINUED, SIGCONT as i32);
    }
    if sig == SIGKILL {
        p.stop_wq.wake_all();
    }
    if wake {
        if let Some(t) = p.task.lock().clone() {
            t.sig_hint.store(true, Ordering::Release);
            t.interrupted.store(true, Ordering::Release);
            crate::sched::wake(&t);
        }
    }
}

pub fn kill(pid: i64, sig: u32) -> KResult<()> {
    if sig as usize >= NSIG {
        return Err(EINVAL);
    }
    let me = proc::current();
    let targets: alloc::vec::Vec<Arc<Process>> = match pid {
        p if p > 0 => proc::by_pid(p as u32).into_iter().collect(),
        0 => proc::all().into_iter().filter(|q| q.pgid.load(Ordering::Relaxed) == me.pgid.load(Ordering::Relaxed)).collect(),
        -1 => proc::all().into_iter().filter(|q| q.pid > 1 && q.pid != me.pid).collect(),
        p => proc::all().into_iter().filter(|q| q.pgid.load(Ordering::Relaxed) as i64 == -p).collect(),
    };
    let alive: alloc::vec::Vec<_> = targets.into_iter().filter(|q| q.state.load(Ordering::Acquire) == proc::ALIVE).collect();
    if alive.is_empty() {
        return Err(ESRCH);
    }
    // Permission: root may signal anyone; others only processes with their user id
    // (and SIGCONT within the session).
    let (uid, euid) = (me.uid.load(Ordering::Relaxed), me.euid.load(Ordering::Relaxed));
    let allowed = |q: &Arc<Process>| {
        euid == 0
            || [uid, euid].contains(&q.uid.load(Ordering::Relaxed))
            || (sig == SIGCONT && q.sid.load(Ordering::Relaxed) == me.sid.load(Ordering::Relaxed))
    };
    let alive: alloc::vec::Vec<_> = alive.into_iter().filter(allowed).collect();
    if alive.is_empty() {
        return Err(EPERM);
    }
    let info = (0, me.pid, uid, 0); // SI_USER
    for q in alive {
        send_info(&q, sig, info);
    }
    Ok(())
}

pub fn kill_pgrp(pgid: u32, sig: u32) {
    for q in proc::all() {
        if q.pgid.load(Ordering::Relaxed) == pgid {
            send(&q, sig);
        }
    }
}

pub fn send_current(sig: u32) {
    if let Some(p) = proc::try_current() {
        send(&p, sig);
    }
}

/// A fault in user code: the signal cannot be ignored or blocked; if it is not handled the
/// process dies.
pub fn force(sig: u32, f: &mut TrapFrame) {
    let p = proc::current();
    {
        let mut s = p.sig.lock();
        let a = &mut s.actions[sig as usize];
        if a.handler == SIG_IGN {
            *a = SigAction::default();
        }
        s.blocked &= !bit(sig);
        s.pending |= bit(sig);
    }
    crate::sched::current_ref().sig_hint.store(true, Ordering::Release);
    deliver_pending(f);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SigInfo {
    signo: i32,
    errno: i32,
    code: i32,
    _pad: [u8; 116],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UContext {
    magic: u64,
    blocked: u64,
    regs: [u64; 22], // the TrapFrame
}

const UC_MAGIC: u64 = 0x4349_5441_5349_4721; // "CITASIG!"

/// Delivers one pending signal, if any, before returning to user mode with `f`.
pub fn deliver_pending(f: &mut TrapFrame) {
    if !crate::sched::current_ref().sig_hint.load(Ordering::Acquire) {
        return; // fast path: nothing can be pending
    }
    let Some(p) = proc::try_current() else { return };
    let t = crate::sched::current();
    let (sig, action, sinfo) = {
        let mut s = p.sig.lock();
        let d = s.deliverable();
        if d == 0 {
            t.interrupted.store(false, Ordering::Release);
            t.sig_hint.store(false, Ordering::Release);
            return;
        }
        let sig = d.trailing_zeros() + 1;
        s.pending &= !bit(sig);
        if s.deliverable() == 0 {
            t.interrupted.store(false, Ordering::Release);
        }
        (sig, s.actions[sig as usize], s.info[sig as usize])
    };
    drop(t);
    if action.handler == SIG_IGN {
        return;
    }
    if action.handler == SIG_DFL {
        if default_ignored(sig) && sig != SIGKILL {
            return;
        }
        if is_stop(sig) {
            stop_current(&p, sig);
            return;
        }
        drop(p);
        proc::exit(sig as i32 | if dumps_core(sig) { 0x80 } else { 0 });
    }
    // Build the frame on the alternate stack if the action asks for it and we are not on it
    // already, else below the interrupted stack (skipping the 128-byte red zone).
    let alt = p.sig.lock().alt_stack;
    let mut sp = match alt {
        Some((base, size)) if action.flags & SA_ONSTACK != 0 && !(base <= f.rsp && f.rsp < base + size) => (base + size) & !15,
        _ => (f.rsp - 128) & !15,
    };
    sp -= core::mem::size_of::<UContext>() as u64;
    let uc_at = sp & !15;
    sp = uc_at - core::mem::size_of::<SigInfo>() as u64;
    let info_at = sp & !15;
    sp = info_at - 8; // the return address (sa_restorer); rsp+8 is then 16-aligned
    let regs: [u64; 22] = unsafe { core::mem::transmute(f.clone()) };
    let saved_mask = {
        let mut s = p.sig.lock();
        s.restore_mask.take().unwrap_or(s.blocked)
    };
    let uc = UContext { magic: UC_MAGIC, blocked: saved_mask, regs };
    let mut info = SigInfo { signo: sig as i32, errno: 0, code: sinfo.0, _pad: [0; 116] };
    // Linux layout: si_pid at offset 16, si_uid at 20, si_status at 24 (the union after a pad).
    info._pad[4..8].copy_from_slice(&sinfo.1.to_le_bytes());
    info._pad[8..12].copy_from_slice(&sinfo.2.to_le_bytes());
    info._pad[12..16].copy_from_slice(&sinfo.3.to_le_bytes());
    let ok = crate::user::write_struct(uc_at, &uc).is_ok() && crate::user::write_struct(info_at, &info).is_ok() && crate::user::put_u64(sp, action.restorer).is_ok();
    if !ok {
        // No room on the stack for the handler: the process dies of SIGSEGV.
        drop(p);
        proc::exit(SIGSEGV as i32 | 0x80);
    }
    {
        let mut s = p.sig.lock();
        s.blocked |= action.mask & !(bit(SIGKILL) | bit(SIGSTOP));
        if action.flags & SA_NODEFER == 0 {
            s.blocked |= bit(sig);
        }
        if action.flags & SA_RESETHAND != 0 {
            s.actions[sig as usize] = SigAction::default();
        }
    }
    f.rip = action.handler;
    f.rsp = sp;
    f.rdi = sig as u64;
    f.rsi = info_at;
    f.rdx = uc_at;
    f.rax = 0;
    f.vector = 0;
}

/// rt_sigreturn: restores what deliver_pending saved (the frame is at the restorer's rsp - 8).
pub fn sigreturn(f: &mut TrapFrame) -> KResult<()> {
    let p = proc::current();
    let ret_at = f.rsp - 8;
    let info_at = (ret_at + 8 + 15) & !15;
    let uc_at = (info_at + core::mem::size_of::<SigInfo>() as u64 + 15) & !15;
    let uc: UContext = crate::user::read_struct(uc_at)?;
    if uc.magic != UC_MAGIC {
        return Err(EFAULT);
    }
    let mut nf: TrapFrame = unsafe { core::mem::transmute(uc.regs) };
    // Only what user code may set: user segments, the flags it may change, IF on.
    nf.cs = gdt::UCODE as u64;
    nf.ss = gdt::UDATA as u64;
    nf.rflags = (nf.rflags & 0x0cd5) | 0x202;
    nf.vector = 0;
    if nf.rip >= kcore::elf::USER_TOP {
        return Err(EFAULT);
    }
    *f = nf;
    p.sig.lock().blocked = uc.blocked & !(bit(SIGKILL) | bit(SIGSTOP));
    crate::sched::current_ref().sig_hint.store(true, Ordering::Release);
    Ok(())
}

/// A blocking system call was woken by a signal. Stop signals with the default action stop the
/// process right here, and signals ignored by default are dropped, so that the call can simply
/// go on afterwards (as on Linux). Returns true if a signal must reach user space first (a
/// handler, or a default action that ends the process): then the call fails with EINTR.
pub fn must_interrupt() -> bool {
    let Some(p) = proc::try_current() else { return false };
    loop {
        let stop = {
            let mut s = p.sig.lock();
            let d = s.deliverable();
            if d == 0 {
                crate::sched::current_ref().interrupted.store(false, Ordering::Release);
                return false;
            }
            let sig = d.trailing_zeros() + 1;
            let a = s.actions[sig as usize];
            if a.handler == SIG_DFL && is_stop(sig) {
                s.pending &= !bit(sig);
                sig
            } else if a.handler == SIG_DFL && default_ignored(sig) {
                s.pending &= !bit(sig);
                continue;
            } else {
                return true;
            }
        };
        stop_current(&p, stop);
    }
}
