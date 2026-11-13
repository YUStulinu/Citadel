//! The console terminal on COM1, with a line discipline.
//!
//! In canonical mode (the default) input is collected into lines: backspace erases, ^U erases
//! the line, ^D ends the input, ^C and ^\ send SIGINT and SIGQUIT to the foreground process
//! group, and characters are echoed. Programs can switch to raw mode with TCSETS (termios), as a
//! line editor would. Output translates "\n" into "\r\n" (ONLCR).
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};

use kcore::errno::*;

use crate::fs::{Inode, Kind, POLLIN, POLLOUT, Stat};
use crate::sched::WaitQueue;
use crate::serial;
use crate::sync::{Once, SpinLock};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 19],
}

const ICRNL: u32 = 0o400;
const OPOST: u32 = 1;
const ONLCR: u32 = 4;
const ISIG: u32 = 1;
const ICANON: u32 = 2;
const ECHO: u32 = 0o10;
const ECHOE: u32 = 0o20;
const VINTR: usize = 0;
const VQUIT: usize = 1;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;
const VMIN: usize = 6;

struct State {
    termios: Termios,
    line: VecDeque<u8>,    // the line being edited (canonical mode)
    ready: VecDeque<u8>,   // what read() can return
    eof_pending: usize,    // ^D seen: that many reads return 0
}

pub struct Tty {
    state: SpinLock<State>,
    pub wq: WaitQueue,
    pub fg_pgrp: AtomicU32,
}

static CONSOLE: Once<Arc<Tty>> = Once::new();

pub fn console() -> Arc<Tty> {
    CONSOLE.get().clone()
}

pub fn init() {
    let mut cc = [0u8; 19];
    cc[VINTR] = 3;
    cc[VQUIT] = 0x1c;
    cc[VERASE] = 0x7f;
    cc[VKILL] = 0x15;
    cc[VEOF] = 4;
    cc[VMIN] = 1;
    let t = Termios { iflag: ICRNL, oflag: OPOST | ONLCR, cflag: 0o277, lflag: ISIG | ICANON | ECHO | ECHOE, line: 0, cc };
    CONSOLE.set(Arc::new(Tty {
        state: SpinLock::new(State { termios: t, line: VecDeque::new(), ready: VecDeque::new(), eof_pending: 0 }),
        wq: WaitQueue::new(),
        fg_pgrp: AtomicU32::new(0),
    }));
}

fn out(c: u8, t: &Termios) {
    if c == b'\n' && t.oflag & OPOST != 0 && t.oflag & ONLCR != 0 {
        serial::putb(serial::COM1, b'\r');
    }
    serial::putb(serial::COM1, c);
}

/// A byte from the serial port (called from the interrupt handler).
pub fn input(mut c: u8) {
    let tty = match CONSOLE.try_get() {
        Some(t) => t.clone(),
        None => return,
    };
    let mut signal = None;
    {
        let mut s = tty.state.lock();
        let t = s.termios;
        if c == b'\r' && t.iflag & ICRNL != 0 {
            c = b'\n';
        }
        if t.lflag & ISIG != 0 && (c == t.cc[VINTR] || c == t.cc[VQUIT]) {
            signal = Some(if c == t.cc[VINTR] { crate::signal::SIGINT } else { crate::signal::SIGQUIT });
            s.line.clear();
            if t.lflag & ECHO != 0 {
                out(b'^', &t);
                out(if c == 3 { b'C' } else { b'\\' }, &t);
                out(b'\n', &t);
            }
        } else if t.lflag & ICANON != 0 {
            if c == t.cc[VERASE] || c == 8 {
                if s.line.pop_back().is_some() && t.lflag & ECHO != 0 {
                    for b in [8, b' ', 8] {
                        out(b, &t);
                    }
                }
            } else if c == t.cc[VKILL] {
                while s.line.pop_back().is_some() {
                    if t.lflag & ECHO != 0 {
                        for b in [8, b' ', 8] {
                            out(b, &t);
                        }
                    }
                }
            } else if c == t.cc[VEOF] {
                if s.line.is_empty() {
                    s.eof_pending += 1;
                } else {
                    let l: VecDeque<u8> = core::mem::take(&mut s.line);
                    s.ready.extend(l);
                }
            } else {
                if t.lflag & ECHO != 0 {
                    out(c, &t);
                }
                s.line.push_back(c);
                if c == b'\n' {
                    let l: VecDeque<u8> = core::mem::take(&mut s.line);
                    s.ready.extend(l);
                }
            }
        } else {
            if t.lflag & ECHO != 0 {
                out(c, &t);
            }
            s.ready.push_back(c);
        }
    }
    if let Some(sig) = signal {
        let pg = tty.fg_pgrp.load(Ordering::Acquire);
        if pg != 0 {
            crate::signal::kill_pgrp(pg, sig);
        }
    }
    tty.wq.wake_all();
}

impl Inode for Tty {
    fn kind(&self) -> Kind {
        Kind::CharDev
    }
    fn stat(&self) -> Stat {
        Stat { dev: 0x30, ino: 5, mode: 0o620, nlink: 1, rdev: (4 << 8) | 64, ..Default::default() }
    }

    fn read_at(&self, _off: u64, buf: &mut [u8]) -> KResult<usize> {
        let mut n = 0;
        let ok = self.wq.wait_until(|| {
            let mut s = self.state.lock();
            if !s.ready.is_empty() {
                let canon = s.termios.lflag & ICANON != 0;
                while n < buf.len() {
                    let Some(c) = s.ready.pop_front() else { break };
                    buf[n] = c;
                    n += 1;
                    if canon && c == b'\n' {
                        break;
                    }
                }
                return true;
            }
            if s.eof_pending > 0 {
                s.eof_pending -= 1;
                return true;
            }
            false
        });
        if !ok {
            return Err(EINTR);
        }
        Ok(n)
    }

    fn write_at(&self, _off: u64, buf: &[u8]) -> KResult<usize> {
        let t = self.state.lock().termios;
        let _g = crate::serial::lock_output();
        for &c in buf {
            out(c, &t);
        }
        Ok(buf.len())
    }

    fn poll(&self) -> u16 {
        let s = self.state.lock();
        POLLOUT | if !s.ready.is_empty() || s.eof_pending > 0 { POLLIN } else { 0 }
    }

    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&self.wq)
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<u64> {
        const TCGETS: u64 = 0x5401;
        const TCSETS: u64 = 0x5402;
        const TCSETSW: u64 = 0x5403;
        const TCSETSF: u64 = 0x5404;
        const TIOCGPGRP: u64 = 0x540f;
        const TIOCSPGRP: u64 = 0x5410;
        const TIOCGWINSZ: u64 = 0x5413;
        const TIOCSCTTY: u64 = 0x540e;
        match cmd {
            TCGETS => {
                let t = self.state.lock().termios;
                crate::user::write_struct(arg, &t)?;
                Ok(0)
            }
            TCSETS | TCSETSW | TCSETSF => {
                let t: Termios = crate::user::read_struct(arg)?;
                let mut s = self.state.lock();
                if cmd == TCSETSF {
                    s.ready.clear();
                    s.line.clear();
                }
                // Leaving canonical mode: what was typed so far becomes readable.
                if t.lflag & ICANON == 0 {
                    let l: VecDeque<u8> = core::mem::take(&mut s.line);
                    s.ready.extend(l);
                }
                s.termios = t;
                Ok(0)
            }
            TIOCGPGRP => {
                crate::user::put_u32(arg, self.fg_pgrp.load(Ordering::Acquire))?;
                Ok(0)
            }
            TIOCSPGRP => {
                self.fg_pgrp.store(crate::user::get_u32(arg)?, Ordering::Release);
                Ok(0)
            }
            TIOCGWINSZ => {
                crate::user::write_struct(arg, &[24u16, 80, 0, 0])?;
                Ok(0)
            }
            TIOCSCTTY => Ok(0),
            _ => Err(ENOTTY),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
