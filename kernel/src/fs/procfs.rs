//! /proc: generated files describing processes and the kernel, in the formats Linux uses, so the
//! same tools (ps, free, uptime) work on both systems.
//!
//! /proc/self, /proc/<pid>/{stat,status,maps}, /proc/meminfo, /proc/uptime, /proc/stat,
//! /proc/cpuinfo, /proc/mounts, /proc/version, /proc/sched (per-CPU scheduler counters).
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::Ordering;

use super::{DirEntry, Inode, InodeRef, Kind, Stat};
use crate::proc;
use crate::sched;
use kcore::errno::*;

const DEV: u64 = 0x40;
const USER_HZ: u64 = 100;

#[derive(Clone, Copy)]
enum Gen {
    MemInfo,
    Uptime,
    CpuStat,
    CpuInfo,
    Mounts,
    Version,
    Sched,
    PidStat(u32),
    PidStatus(u32),
    PidMaps(u32),
}

struct ProcFile {
    kind: Gen,
    ino: u64,
}

fn ticks(ns: u64) -> u64 {
    ns / (1_000_000_000 / USER_HZ)
}

fn state_char(p: &proc::Process) -> char {
    if p.state.load(Ordering::Acquire) == proc::ZOMBIE {
        return 'Z';
    }
    match p.task.lock().as_ref().map(|t| t.state.load(Ordering::Acquire)) {
        Some(sched::SLEEPING) => 'S',
        Some(sched::DEAD) => 'X',
        _ => 'R',
    }
}

fn runtime_ns(p: &proc::Process) -> u64 {
    p.task.lock().as_ref().map(|t| t.runtime_ns.load(Ordering::Relaxed)).unwrap_or(0) + p.utime_ns.load(Ordering::Relaxed)
}

fn generate(g: Gen) -> KResult<String> {
    let mut s = String::new();
    match g {
        Gen::MemInfo => {
            let total = crate::mm::frame::total_bytes() / 1024;
            let free = crate::mm::frame::free_bytes() / 1024;
            let _ = write!(s, "MemTotal:       {total:8} kB\nMemFree:        {free:8} kB\nMemAvailable:   {free:8} kB\n");
        }
        Gen::Uptime => {
            let up = crate::arch::apic::now_ns();
            let idle: u64 = sched::stats().iter().map(|x| x.2).sum();
            let _ = writeln!(s, "{}.{:02} {}.{:02}", up / 1_000_000_000, up / 10_000_000 % 100, idle / 1_000_000_000, idle / 10_000_000 % 100);
        }
        Gen::CpuStat => {
            let st = sched::stats();
            let up = crate::arch::apic::now_ns();
            let mut tot_busy = 0;
            let mut tot_idle = 0;
            let mut lines = String::new();
            for (i, (_, _, idle, _)) in st.iter().enumerate() {
                let busy = up.saturating_sub(*idle);
                tot_busy += busy;
                tot_idle += *idle;
                let _ = writeln!(lines, "cpu{i} 0 0 {} {} 0 0 0 0 0 0", ticks(busy), ticks(*idle));
            }
            let _ = writeln!(s, "cpu  0 0 {} {} 0 0 0 0 0 0", ticks(tot_busy), ticks(tot_idle));
            s.push_str(&lines);
            let switches: u64 = st.iter().map(|x| x.1).sum();
            let _ = writeln!(s, "ctxt {switches}\nprocesses {}\nprocs_running {}", proc::all().len(), st.iter().map(|x| x.0).sum::<usize>());
        }
        Gen::CpuInfo => {
            let mhz = crate::arch::apic::tsc_mhz();
            let (vendor, brand) = (crate::arch::cpu::vendor(), crate::arch::cpu::brand());
            for i in 0..crate::arch::percpu::online() {
                let _ = write!(s, "processor\t: {i}\nvendor_id\t: {vendor}\nmodel name\t: {brand}\ncpu MHz\t\t: {mhz}\n\n");
            }
        }
        Gen::Mounts => {
            for (path, fs) in super::mounts() {
                let _ = writeln!(s, "{fs} {path} {fs} rw 0 0");
            }
        }
        Gen::Version => {
            let _ = writeln!(s, "Citadel version {} #1 SMP", env!("CARGO_PKG_VERSION"));
        }
        Gen::Sched => {
            let _ = writeln!(s, "cpu queued switches idle_ms ticks");
            for (i, (q, sw, idle, t)) in sched::stats().iter().enumerate() {
                let _ = writeln!(s, "{i} {q} {sw} {} {t}", idle / 1_000_000);
            }
        }
        Gen::PidStat(pid) | Gen::PidStatus(pid) | Gen::PidMaps(pid) => {
            let p = proc::by_pid(pid).ok_or(ENOENT)?;
            let name = p.name.lock().clone();
            let (vsize, rss, vmas) = {
                let mm = p.mm.lock();
                let v: u64 = mm.vmas.iter().map(|a| a.end - a.start).sum();
                (v, crate::mm::paging::resident_pages(mm.cr3), mm.vmas.clone())
            };
            let state = state_char(&p);
            let ppid = p.ppid.load(Ordering::Relaxed);
            let run = ticks(runtime_ns(&p));
            match g {
                Gen::PidStat(_) => {
                    let _ = writeln!(
                        s,
                        "{pid} ({name}) {state} {ppid} {} {} 0 -1 0 0 0 0 0 {run} 0 0 0 20 0 1 0 {} {vsize} {rss} 18446744073709551615",
                        p.pgid.load(Ordering::Relaxed),
                        p.sid.load(Ordering::Relaxed),
                        ticks(p.start_ns)
                    );
                }
                Gen::PidStatus(_) => {
                    let _ = write!(
                        s,
                        "Name:\t{name}\nState:\t{state}\nPid:\t{pid}\nPPid:\t{ppid}\nVmSize:\t{} kB\nVmRSS:\t{} kB\nThreads:\t1\n",
                        vsize / 1024,
                        rss * 4
                    );
                }
                _ => {
                    for v in vmas {
                        let r = if v.prot & proc::PROT_READ != 0 { 'r' } else { '-' };
                        let w = if v.prot & proc::PROT_WRITE != 0 { 'w' } else { '-' };
                        let x = if v.prot & proc::PROT_EXEC != 0 { 'x' } else { '-' };
                        let tag = if v.grows_down { "[stack]" } else { "" };
                        let _ = writeln!(s, "{:012x}-{:012x} {r}{w}{x}p 00000000 00:00 0 {tag}", v.start, v.end);
                    }
                }
            }
        }
    }
    Ok(s)
}

impl Inode for ProcFile {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
    fn kind(&self) -> Kind {
        Kind::File
    }
    fn stat(&self) -> Stat {
        Stat { dev: DEV, ino: self.ino, mode: 0o444, nlink: 1, ..Default::default() }
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        let text = generate(self.kind)?;
        let b = text.as_bytes();
        let off = off as usize;
        if off >= b.len() {
            return Ok(0);
        }
        let n = buf.len().min(b.len() - off);
        buf[..n].copy_from_slice(&b[off..off + n]);
        Ok(n)
    }
}

struct SelfLink;

impl Inode for SelfLink {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
    fn kind(&self) -> Kind {
        Kind::Symlink
    }
    fn stat(&self) -> Stat {
        Stat { dev: DEV, ino: 2, mode: 0o777, nlink: 1, ..Default::default() }
    }
    fn readlink(&self) -> KResult<String> {
        Ok(format!("{}", proc::current().pid))
    }
}

struct PidDir {
    pid: u32,
}

const PID_FILES: [&str; 3] = ["stat", "status", "maps"];

fn pid_base(pid: u32) -> u64 {
    1000 + pid as u64 * 16
}

impl Inode for PidDir {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
    fn kind(&self) -> Kind {
        Kind::Dir
    }
    fn stat(&self) -> Stat {
        Stat { dev: DEV, ino: pid_base(self.pid), mode: 0o555, nlink: 2, ..Default::default() }
    }
    fn lookup(&self, name: &str) -> KResult<InodeRef> {
        proc::by_pid(self.pid).ok_or(ENOENT)?;
        let g = match name {
            "stat" => Gen::PidStat(self.pid),
            "status" => Gen::PidStatus(self.pid),
            "maps" => Gen::PidMaps(self.pid),
            _ => return Err(ENOENT),
        };
        let idx = PID_FILES.iter().position(|f| *f == name).unwrap_or(0) as u64;
        Ok(Arc::new(ProcFile { kind: g, ino: pid_base(self.pid) + 1 + idx }))
    }
    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        let base = pid_base(self.pid);
        Ok(PID_FILES.iter().enumerate().map(|(i, n)| DirEntry { name: String::from(*n), ino: base + 1 + i as u64, kind: Kind::File }).collect())
    }
}

pub struct ProcRoot;

const ROOT_FILES: [(&str, Gen); 7] = [
    ("meminfo", Gen::MemInfo),
    ("uptime", Gen::Uptime),
    ("stat", Gen::CpuStat),
    ("cpuinfo", Gen::CpuInfo),
    ("mounts", Gen::Mounts),
    ("version", Gen::Version),
    ("sched", Gen::Sched),
];

impl ProcRoot {
    pub fn new() -> InodeRef {
        Arc::new(ProcRoot)
    }
}

impl Inode for ProcRoot {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
    fn kind(&self) -> Kind {
        Kind::Dir
    }
    fn stat(&self) -> Stat {
        Stat { dev: DEV, ino: 1, mode: 0o555, nlink: 2, ..Default::default() }
    }
    fn lookup(&self, name: &str) -> KResult<InodeRef> {
        if name == "self" {
            return Ok(Arc::new(SelfLink));
        }
        if let Some(i) = ROOT_FILES.iter().position(|f| f.0 == name) {
            return Ok(Arc::new(ProcFile { kind: ROOT_FILES[i].1, ino: 10 + i as u64 }));
        }
        let pid: u32 = name.parse().map_err(|_| ENOENT)?;
        proc::by_pid(pid).ok_or(ENOENT)?;
        Ok(Arc::new(PidDir { pid }))
    }
    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        let mut v: Vec<DirEntry> = ROOT_FILES.iter().enumerate().map(|(i, f)| DirEntry { name: String::from(f.0), ino: 10 + i as u64, kind: Kind::File }).collect();
        v.push(DirEntry { name: String::from("self"), ino: 2, kind: Kind::Symlink });
        for p in proc::all() {
            v.push(DirEntry { name: format!("{}", p.pid), ino: pid_base(p.pid), kind: Kind::Dir });
        }
        Ok(v)
    }
}
