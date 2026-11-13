//! POSIX record locks (fcntl F_GETLK / F_SETLK / F_SETLKW). Locks belong to a process and an
//! inode; they are not inherited by fork, and closing any descriptor of the file, or exiting,
//! releases all of the process's locks on it.
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use kcore::errno::*;

use super::{File, InodeRef};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

const F_GETLK: u32 = 5;
const F_SETLK: u32 = 6;
const F_RDLCK: i16 = 0;
const F_WRLCK: i16 = 1;
const F_UNLCK: i16 = 2;

#[derive(Clone, Copy)]
struct Lock {
    pid: u32,
    start: u64,
    end: u64, // exclusive; u64::MAX = to the end of the file and beyond
    write: bool,
}

static LOCKS: SpinLock<BTreeMap<usize, Vec<Lock>>> = SpinLock::new(BTreeMap::new());
static WAITERS: WaitQueue = WaitQueue::new();

fn key(i: &InodeRef) -> usize {
    Arc::as_ptr(i) as *const u8 as usize
}

fn conflict(locks: &[Lock], pid: u32, start: u64, end: u64, write: bool) -> Option<Lock> {
    locks.iter().copied().find(|l| l.pid != pid && l.start < end && start < l.end && (write || l.write))
}

/// Replaces the process's locks over [start, end) by `new` (None = unlock).
fn apply(locks: &mut Vec<Lock>, pid: u32, start: u64, end: u64, new: Option<bool>) {
    let mut out = Vec::new();
    for l in locks.drain(..) {
        if l.pid != pid || l.end <= start || end <= l.start {
            out.push(l);
            continue;
        }
        if l.start < start {
            out.push(Lock { end: start, ..l });
        }
        if end < l.end {
            out.push(Lock { start: end, ..l });
        }
    }
    if let Some(write) = new {
        out.push(Lock { pid, start, end, write });
    }
    *locks = out;
}

pub fn fcntl(f: &Arc<File>, cmd: u32, arg: u64) -> KResult<u64> {
    let mut b = [0u8; 32];
    crate::user::copy_in(arg, &mut b)?;
    let ltype = i16::from_le_bytes([b[0], b[1]]);
    let whence = i16::from_le_bytes([b[2], b[3]]);
    let lstart = i64::from_le_bytes(b[8..16].try_into().unwrap());
    let llen = i64::from_le_bytes(b[16..24].try_into().unwrap());
    let base = match whence {
        0 => 0,
        1 => *f.offset.lock() as i64,
        2 => f.inode.stat().size as i64,
        _ => return Err(EINVAL),
    };
    let mut start = base + lstart;
    let mut end = if llen == 0 { i64::MAX } else { start + llen };
    if llen < 0 {
        end = start;
        start += llen;
    }
    if start < 0 {
        return Err(EINVAL);
    }
    let (start, end) = (start as u64, if end == i64::MAX { u64::MAX } else { end as u64 });
    let write = match ltype {
        F_RDLCK => false,
        F_WRLCK => true,
        F_UNLCK => false,
        _ => return Err(EINVAL),
    };
    if ltype == F_RDLCK && !f.readable() || ltype == F_WRLCK && !f.writable() {
        if cmd != F_GETLK {
            return Err(EBADF);
        }
    }
    let pid = crate::proc::current().pid;
    let k = key(&f.inode);
    if cmd == F_GETLK {
        let found = LOCKS.lock().get(&k).and_then(|v| conflict(v, pid, start, end, write));
        match found {
            Some(l) => {
                b[0..2].copy_from_slice(&(if l.write { F_WRLCK } else { F_RDLCK }).to_le_bytes());
                b[2..4].copy_from_slice(&0i16.to_le_bytes());
                b[8..16].copy_from_slice(&(l.start as i64).to_le_bytes());
                let len = if l.end == u64::MAX { 0 } else { (l.end - l.start) as i64 };
                b[16..24].copy_from_slice(&len.to_le_bytes());
                b[24..28].copy_from_slice(&l.pid.to_le_bytes());
            }
            None => b[0..2].copy_from_slice(&F_UNLCK.to_le_bytes()),
        }
        crate::user::copy_out(arg, &b)?;
        return Ok(0);
    }
    loop {
        {
            let mut all = LOCKS.lock();
            let v = all.entry(k).or_default();
            if ltype == F_UNLCK {
                apply(v, pid, start, end, None);
                drop(all);
                WAITERS.wake_all();
                return Ok(0);
            }
            if conflict(v, pid, start, end, write).is_none() {
                apply(v, pid, start, end, Some(write));
                return Ok(0);
            }
        }
        if cmd == F_SETLK {
            return Err(EAGAIN);
        }
        let free = || LOCKS.lock().get(&k).is_none_or(|v| conflict(v, pid, start, end, write).is_none());
        if !WAITERS.wait_until(free) {
            return Err(EINTR);
        }
    }
}

/// The process closed a descriptor of `inode`: its locks on the file go.
pub fn release(pid: u32, inode: &InodeRef) {
    let k = key(inode);
    let mut all = LOCKS.lock();
    if let Some(v) = all.get_mut(&k) {
        let before = v.len();
        v.retain(|l| l.pid != pid);
        if v.is_empty() {
            all.remove(&k);
        }
        if before != 0 {
            drop(all);
            WAITERS.wake_all();
        }
    }
}

/// The process exited.
pub fn release_all(pid: u32) {
    let mut all = LOCKS.lock();
    let mut changed = false;
    all.retain(|_, v| {
        let n = v.len();
        v.retain(|l| l.pid != pid);
        changed |= v.len() != n;
        !v.is_empty()
    });
    drop(all);
    if changed {
        WAITERS.wake_all();
    }
}
