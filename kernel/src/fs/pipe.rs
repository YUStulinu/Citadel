//! Pipes: a 64 KiB ring buffer between a read end and a write end. Reading an empty pipe
//! blocks until data arrives or every writer is gone (end of file); writing to a pipe with no
//! readers fails with EPIPE (and SIGPIPE). Named pipes (FIFOs) hand out ends when opened.
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};

use kcore::errno::*;

use super::*;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

pub const CAPACITY: usize = 64 * 1024;

pub struct PipeInner {
    buf: SpinLock<VecDeque<u8>>,
    readers: AtomicU32,
    writers: AtomicU32,
    pub wq: WaitQueue,
    ino: u64,
}

impl PipeInner {
    fn new() -> Arc<PipeInner> {
        Arc::new(PipeInner { buf: SpinLock::new(VecDeque::new()), readers: AtomicU32::new(0), writers: AtomicU32::new(0), wq: WaitQueue::new(), ino: new_ino() })
    }
}

pub struct PipeEnd {
    pub inner: Arc<PipeInner>,
    write: bool,
    pub nonblock: core::sync::atomic::AtomicBool,
}

impl PipeEnd {
    fn new(inner: Arc<PipeInner>, write: bool) -> Arc<PipeEnd> {
        if write {
            inner.writers.fetch_add(1, Ordering::AcqRel);
        } else {
            inner.readers.fetch_add(1, Ordering::AcqRel);
        }
        Arc::new(PipeEnd { inner, write, nonblock: core::sync::atomic::AtomicBool::new(false) })
    }
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        if self.write {
            self.inner.writers.fetch_sub(1, Ordering::AcqRel);
        } else {
            self.inner.readers.fetch_sub(1, Ordering::AcqRel);
        }
        self.inner.wq.wake_all();
    }
}

/// A new pipe: (read end, write end).
pub fn pipe() -> (Arc<PipeEnd>, Arc<PipeEnd>) {
    let p = PipeInner::new();
    (PipeEnd::new(p.clone(), false), PipeEnd::new(p, true))
}

impl Inode for PipeEnd {
    fn kind(&self) -> Kind {
        Kind::Fifo
    }
    fn stat(&self) -> Stat {
        Stat { dev: 0x20, ino: self.inner.ino, mode: 0o600, nlink: 1, size: self.inner.buf.lock().len() as u64, ..Default::default() }
    }

    fn read_at(&self, _off: u64, buf: &mut [u8]) -> KResult<usize> {
        if self.write {
            return Err(EBADF);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let p = &self.inner;
        let mut out = 0;
        let ok = p.wq.wait_until(|| {
            let mut b = p.buf.lock();
            if !b.is_empty() {
                let n = buf.len().min(b.len());
                for (i, x) in b.drain(..n).enumerate() {
                    buf[i] = x;
                }
                out = n;
                return true;
            }
            p.writers.load(Ordering::Acquire) == 0 || self.nonblock.load(Ordering::Relaxed)
        });
        if !ok {
            return Err(EINTR);
        }
        if out == 0 && self.nonblock.load(Ordering::Relaxed) && p.writers.load(Ordering::Acquire) > 0 {
            return Err(EAGAIN);
        }
        p.wq.wake_all();
        Ok(out)
    }

    fn write_at(&self, _off: u64, buf: &[u8]) -> KResult<usize> {
        if !self.write {
            return Err(EBADF);
        }
        let p = &self.inner;
        let mut done = 0;
        while done < buf.len() {
            let mut broken = false;
            let mut wrote = 0;
            let ok = p.wq.wait_until(|| {
                if p.readers.load(Ordering::Acquire) == 0 {
                    broken = true;
                    return true;
                }
                let mut b = p.buf.lock();
                let room = CAPACITY - b.len();
                if room > 0 {
                    let n = room.min(buf.len() - done);
                    b.extend(&buf[done..done + n]);
                    wrote = n;
                    return true;
                }
                self.nonblock.load(Ordering::Relaxed)
            });
            if !ok {
                return if done > 0 { Ok(done) } else { Err(EINTR) };
            }
            if broken {
                crate::signal::send_current(crate::signal::SIGPIPE);
                return if done > 0 { Ok(done) } else { Err(EPIPE) };
            }
            if wrote == 0 {
                return if done > 0 { Ok(done) } else { Err(EAGAIN) };
            }
            done += wrote;
            p.wq.wake_all();
        }
        Ok(done)
    }

    fn poll(&self) -> u16 {
        let p = &self.inner;
        let len = p.buf.lock().len();
        if self.write {
            if p.readers.load(Ordering::Acquire) == 0 {
                POLLERR | POLLOUT
            } else if len < CAPACITY {
                POLLOUT
            } else {
                0
            }
        } else {
            let mut r = 0;
            if len > 0 {
                r |= POLLIN;
            }
            if p.writers.load(Ordering::Acquire) == 0 {
                r |= POLLHUP | POLLIN;
            }
            r
        }
    }

    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&self.inner.wq)
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<u64> {
        const FIONREAD: u64 = 0x541b;
        if cmd == FIONREAD {
            crate::user::put_u32(arg, self.inner.buf.lock().len() as u32)?;
            return Ok(0);
        }
        Err(ENOTTY)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A named pipe in a directory: opening it gives a read or write end of its one pipe.
pub struct Fifo {
    inner: Arc<PipeInner>,
}

impl Pipe {
    pub fn new_fifo() -> InodeRef {
        Arc::new(Fifo { inner: PipeInner::new() })
    }
}

pub struct Pipe;

impl Fifo {
    pub fn open_end(&self, write: bool) -> Arc<PipeEnd> {
        PipeEnd::new(self.inner.clone(), write)
    }
}

impl Inode for Fifo {
    fn kind(&self) -> Kind {
        Kind::Fifo
    }
    fn stat(&self) -> Stat {
        Stat { dev: 0x20, ino: self.inner.ino, mode: 0o644, nlink: 1, ..Default::default() }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
