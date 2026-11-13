//! /dev: null, zero, full, random, urandom, and the console (tty, console, ttyS0).
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use kcore::errno::*;

use super::*;

#[derive(Clone, Copy, PartialEq)]
enum Dev {
    Null,
    Zero,
    Full,
    Random,
}

struct DevInode {
    dev: Dev,
    ino: u64,
    rdev: u64,
}

static SEED: AtomicU64 = AtomicU64::new(0x9e37_79b9_7f4a_7c15);

/// Random bytes: RDRAND when the CPU has it, mixed with the TSC through xorshift otherwise.
pub fn random_u64() -> u64 {
    let mut v: u64 = 0;
    let ok: u8;
    if crate::arch::cpu::cpuid(1, 0).ecx & (1 << 30) != 0 {
        unsafe { core::arch::asm!("rdrand {0}", "setc {1}", out(reg) v, out(reg_byte) ok) };
        if ok != 0 {
            return v;
        }
    }
    let mut x = SEED.load(Ordering::Relaxed) ^ crate::arch::cpu::rdtsc();
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    SEED.store(x, Ordering::Relaxed);
    x
}

pub fn fill_random(buf: &mut [u8]) {
    for chunk in buf.chunks_mut(8) {
        let r = random_u64().to_le_bytes();
        chunk.copy_from_slice(&r[..chunk.len()]);
    }
}

impl Inode for DevInode {
    fn kind(&self) -> Kind {
        Kind::CharDev
    }
    fn stat(&self) -> Stat {
        Stat { dev: 0x30, ino: self.ino, mode: 0o666, nlink: 1, rdev: self.rdev, ..Default::default() }
    }
    fn read_at(&self, _off: u64, buf: &mut [u8]) -> KResult<usize> {
        match self.dev {
            Dev::Null => Ok(0),
            Dev::Zero | Dev::Full => {
                buf.fill(0);
                Ok(buf.len())
            }
            Dev::Random => {
                fill_random(buf);
                Ok(buf.len())
            }
        }
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> KResult<usize> {
        match self.dev {
            Dev::Full => Err(ENOSPC),
            _ => Ok(buf.len()),
        }
    }
    fn truncate(&self, _len: u64) -> KResult<()> {
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct DevDir {
    entries: Vec<(String, InodeRef)>,
}

impl DevDir {
    pub fn new() -> InodeRef {
        let d = |dev, ino, rdev| -> InodeRef { Arc::new(DevInode { dev, ino, rdev }) };
        let tty: InodeRef = crate::tty::console();
        let entries = alloc::vec![
            (String::from("null"), d(Dev::Null, 1, (1 << 8) | 3)),
            (String::from("zero"), d(Dev::Zero, 2, (1 << 8) | 5)),
            (String::from("full"), d(Dev::Full, 3, (1 << 8) | 7)),
            (String::from("random"), d(Dev::Random, 4, (1 << 8) | 8)),
            (String::from("urandom"), d(Dev::Random, 6, (1 << 8) | 9)),
            (String::from("tty"), tty.clone()),
            (String::from("console"), tty.clone()),
            (String::from("ttyS0"), tty),
        ];
        Arc::new(DevDir { entries })
    }
}

impl Inode for DevDir {
    fn kind(&self) -> Kind {
        Kind::Dir
    }
    fn stat(&self) -> Stat {
        Stat { dev: 0x30, ino: 100, mode: 0o755, nlink: 2, ..Default::default() }
    }
    fn lookup(&self, name: &str) -> KResult<InodeRef> {
        self.entries.iter().find(|(n, _)| n == name).map(|(_, i)| i.clone()).ok_or(ENOENT)
    }
    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        Ok(self.entries.iter().map(|(n, i)| DirEntry { name: n.clone(), ino: i.stat().ino, kind: i.kind() }).collect())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
