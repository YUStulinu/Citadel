//! Processes: an address space with its memory areas, open files, a working directory,
//! signal state, children, and one thread (a scheduler task).
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};

use kcore::errno::*;

use crate::arch::trap::TrapFrame;
use crate::fs::File;
use crate::mm::paging;
use crate::sched::{self, Task, WaitQueue};
use crate::signal::SigState;
use crate::sync::SpinLock;

pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;

pub const STACK_TOP: u64 = 0x0000_7fff_ffff_e000;
pub const STACK_MAX: u64 = 8 << 20;
pub const MMAP_TOP: u64 = 0x0000_7f00_0000_0000;

#[derive(Clone, Debug)]
pub struct Vma {
    pub start: u64,
    pub end: u64,
    pub prot: u32,
    pub grows_down: bool,
    /// madvise(MADV_NOHUGEPAGE): fault this area in 4 KiB pages only.
    pub nohuge: bool,
}

pub struct Mm {
    pub cr3: u64,
    pub vmas: Vec<Vma>,
    pub brk_start: u64,
    pub brk: u64,
    pub mmap_next: u64,
}

impl Mm {
    pub fn find(&self, addr: u64) -> Option<&Vma> {
        self.vmas.iter().find(|v| v.start <= addr && addr < v.end)
    }

    /// Adds an area, replacing whatever was mapped in that range.
    pub fn insert(&mut self, vma: Vma) {
        self.remove_range(vma.start, vma.end);
        self.vmas.push(vma);
        self.vmas.sort_by_key(|v| v.start);
    }

    /// Removes [start, end) from the areas (splitting them) and unmaps the pages.
    pub fn remove_range(&mut self, start: u64, end: u64) {
        let mut out = Vec::with_capacity(self.vmas.len() + 1);
        for v in self.vmas.drain(..) {
            if v.end <= start || v.start >= end {
                out.push(v);
                continue;
            }
            if v.start < start {
                out.push(Vma { end: start, ..v.clone() });
            }
            if v.end > end {
                out.push(Vma { start: end, ..v.clone() });
            }
        }
        self.vmas = out;
        paging::unmap_range(self.cr3, start, end);
    }

    /// A free range of `len` bytes below the mmap area's current bottom.
    pub fn find_free(&mut self, len: u64) -> u64 {
        let mut top = self.mmap_next;
        loop {
            let start = top - len;
            match self.vmas.iter().find(|v| v.start < top && v.end > start) {
                Some(v) => top = v.start,
                None => {
                    self.mmap_next = start;
                    return start;
                }
            }
        }
    }
}

pub struct FdEntry {
    pub file: Arc<File>,
    pub cloexec: bool,
}

pub const MAX_FDS: usize = 1024;

#[derive(Default)]
pub struct FdTable {
    pub fds: Vec<Option<FdEntry>>,
}

impl FdTable {
    pub fn get(&self, fd: i64) -> KResult<Arc<File>> {
        if fd < 0 {
            return Err(EBADF);
        }
        self.fds.get(fd as usize).and_then(|e| e.as_ref()).map(|e| e.file.clone()).ok_or(EBADF)
    }
    /// The lowest free descriptor >= min.
    pub fn alloc(&mut self, file: Arc<File>, cloexec: bool, min: usize) -> KResult<i64> {
        let mut i = min;
        while i < self.fds.len() && self.fds[i].is_some() {
            i += 1;
        }
        if i >= MAX_FDS {
            return Err(EMFILE);
        }
        if i >= self.fds.len() {
            self.fds.resize_with(i + 1, || None);
        }
        self.fds[i] = Some(FdEntry { file, cloexec });
        Ok(i as i64)
    }
    pub fn set(&mut self, fd: usize, file: Arc<File>, cloexec: bool) -> KResult<()> {
        if fd >= MAX_FDS {
            return Err(EBADF);
        }
        if fd >= self.fds.len() {
            self.fds.resize_with(fd + 1, || None);
        }
        self.fds[fd] = Some(FdEntry { file, cloexec });
        Ok(())
    }
    pub fn close(&mut self, fd: i64) -> KResult<()> {
        if fd < 0 {
            return Err(EBADF);
        }
        match self.fds.get_mut(fd as usize) {
            Some(e @ Some(_)) => {
                *e = None;
                Ok(())
            }
            _ => Err(EBADF),
        }
    }
}

pub const ALIVE: u8 = 0;
pub const ZOMBIE: u8 = 1;

pub struct Process {
    pub pid: u32,
    pub ppid: AtomicU32,
    pub pgid: AtomicU32,
    pub sid: AtomicU32,
    pub name: SpinLock<String>,
    pub mm: SpinLock<Mm>,
    pub fds: SpinLock<FdTable>,
    pub cwd: SpinLock<String>,
    pub umask: AtomicU32,
    pub children: SpinLock<Vec<Arc<Process>>>,
    pub state: AtomicU8,
    /// wait() status: (code << 8) for exit, the signal number for a kill.
    pub wstatus: AtomicI32,
    pub child_wq: WaitQueue,
    pub sig: SpinLock<SigState>,
    pub task: SpinLock<Option<Arc<Task>>>,
    pub start_ns: u64,
    pub utime_ns: AtomicU64,
}

static NEXT_PID: AtomicU32 = AtomicU32::new(1);
pub static TABLE: SpinLock<BTreeMap<u32, Arc<Process>>> = SpinLock::new(BTreeMap::new());

pub fn current() -> Arc<Process> {
    sched::current().process.lock().clone().expect("not in a process")
}

pub fn try_current() -> Option<Arc<Process>> {
    sched::try_current().and_then(|t| t.process.lock().clone())
}

pub fn by_pid(pid: u32) -> Option<Arc<Process>> {
    TABLE.lock().get(&pid).cloned()
}

pub fn all() -> Vec<Arc<Process>> {
    TABLE.lock().values().cloned().collect()
}

fn new_process(ppid: u32, cr3: u64, name: &str) -> Arc<Process> {
    let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
    let p = Arc::new(Process {
        pid,
        ppid: AtomicU32::new(ppid),
        pgid: AtomicU32::new(pid),
        sid: AtomicU32::new(pid),
        name: SpinLock::new(String::from(name)),
        mm: SpinLock::new(Mm { cr3, vmas: Vec::new(), brk_start: 0, brk: 0, mmap_next: MMAP_TOP }),
        fds: SpinLock::new(FdTable::default()),
        cwd: SpinLock::new(String::from("/")),
        umask: AtomicU32::new(0o022),
        children: SpinLock::new(Vec::new()),
        state: AtomicU8::new(ALIVE),
        wstatus: AtomicI32::new(0),
        child_wq: WaitQueue::new(),
        sig: SpinLock::new(SigState::default()),
        task: SpinLock::new(None),
        start_ns: crate::time::monotonic_ns(),
        utime_ns: AtomicU64::new(0),
    });
    TABLE.lock().insert(pid, p.clone());
    p
}

/// Enters user mode with `frame` from the top of the current task's kernel stack.
pub fn enter_user(frame: TrapFrame) -> ! {
    let t = sched::current();
    let top = t.kstack_top();
    let at = top - core::mem::size_of::<TrapFrame>() as u64;
    drop(t);
    // The frame goes at the top of the stack we are running on, possibly over `frame` itself and
    // over live call frames, so the copy is done in assembly with no stack use (memmove order).
    let src = &frame as *const TrapFrame as u64;
    const WORDS: u64 = (core::mem::size_of::<TrapFrame>() / 8) as u64;
    unsafe {
        core::arch::asm!(
            "cli",
            "cmp rdi, rsi",
            "jbe 2f",
            "lea rsi, [rsi + {last}]",
            "lea rdi, [rdi + {last}]",
            "std",
            "2:",
            "rep movsq",
            "cld",
            "mov rsp, {at}",
            "jmp {ret}",
            last = const (WORDS - 1) * 8,
            at = in(reg) at,
            ret = in(reg) crate::arch::trap::trap_return_address(),
            in("rdi") at,
            in("rsi") src,
            in("rcx") WORDS,
            options(noreturn)
        );
    }
}

/// Starts process 1 from `path` with the console on fds 0, 1, 2.
pub fn spawn_init(path: &str, argv: &[&str]) {
    let cr3 = paging::new_address_space().expect("no memory for init");
    let p = new_process(0, cr3, "init");
    let tty: crate::fs::InodeRef = crate::tty::console();
    {
        let mut fds = p.fds.lock();
        for _ in 0..3 {
            fds.alloc(File::new(tty.clone(), crate::fs::O_RDWR, String::from("/dev/console")), false, 0).unwrap();
        }
    }
    crate::tty::console().fg_pgrp.store(p.pid, Ordering::Release);
    let path = String::from(path);
    let argv: Vec<String> = argv.iter().map(|s| String::from(*s)).collect();
    let pc = p.clone();
    let task = sched::create_task("init", move || {
        let env = [String::from("PATH=/bin"), String::from("HOME=/root"), String::from("TERM=vt100")];
        let mut f = TrapFrame::default();
        let r = crate::exec::exec(&pc, &path, &argv, &env, &mut f);
        drop(pc);
        match r {
            Ok(()) => enter_user(f),
            Err(e) => panic!("cannot start {path}: errno {e}"),
        }
    });
    task.process.lock().replace(p.clone());
    task.cr3.store(cr3, Ordering::Release);
    *p.task.lock() = Some(task.clone());
    sched::enqueue(task);
}

/// fork(): a copy of the current process (copy-on-write memory) whose thread returns 0.
pub fn fork(parent_frame: &TrapFrame) -> KResult<u32> {
    let parent = current();
    let cr3 = {
        let mm = parent.mm.lock();
        paging::fork_address_space(mm.cr3).ok_or(ENOMEM)?
    };
    let name = parent.name.lock().clone();
    let child = new_process(parent.pid, cr3, &name);
    {
        let pm = parent.mm.lock();
        let mut cm = child.mm.lock();
        cm.vmas = pm.vmas.clone();
        cm.brk_start = pm.brk_start;
        cm.brk = pm.brk;
        cm.mmap_next = pm.mmap_next;
    }
    {
        let pf = parent.fds.lock();
        let mut cf = child.fds.lock();
        cf.fds = pf.fds.iter().map(|e| e.as_ref().map(|e| FdEntry { file: e.file.clone(), cloexec: e.cloexec })).collect();
    }
    *child.cwd.lock() = parent.cwd.lock().clone();
    child.umask.store(parent.umask.load(Ordering::Relaxed), Ordering::Relaxed);
    child.pgid.store(parent.pgid.load(Ordering::Relaxed), Ordering::Relaxed);
    child.sid.store(parent.sid.load(Ordering::Relaxed), Ordering::Relaxed);
    {
        let ps = parent.sig.lock();
        child.sig.lock().inherit_from(&ps);
    }
    parent.children.lock().push(child.clone());

    let mut frame = parent_frame.clone();
    frame.rax = 0;
    frame.vector = 0; // return through iretq
    let fs_base = crate::arch::cpu::rdmsr(crate::arch::cpu::MSR_FS_BASE);
    let task = sched::create_task(&name, move || enter_user(frame));
    task.fs_base.store(fs_base, Ordering::Relaxed);
    task.affinity.store(sched::current().affinity.load(Ordering::Relaxed), Ordering::Relaxed);
    unsafe {
        // The child continues with the parent's FPU/SSE state.
        core::arch::asm!("fxsave64 [{}]", in(reg) (*task.fpu.get()).0.as_mut_ptr());
    }
    task.process.lock().replace(child.clone());
    task.cr3.store(cr3, Ordering::Release);
    *child.task.lock() = Some(task.clone());
    sched::enqueue(task);
    Ok(child.pid)
}

/// Ends the current process with a wait status; never returns.
pub fn exit(wstatus: i32) -> ! {
    let p = current();
    if p.pid == 1 {
        panic!("init exited (status {wstatus:#x})");
    }
    // Close files and release memory now; the zombie keeps only its exit status.
    let files: Vec<Option<FdEntry>> = core::mem::take(&mut p.fds.lock().fds);
    drop(files);
    {
        let cr3 = {
            let mut mm = p.mm.lock();
            mm.vmas.clear();
            core::mem::replace(&mut mm.cr3, paging::kernel_pml4())
        };
        // Run on the kernel's page tables while the process's are freed.
        sched::current().cr3.store(paging::kernel_pml4(), Ordering::Release);
        sched::load_cr3(paging::kernel_pml4());
        sched::evict_lazy_cr3(cr3);
        paging::free_address_space(cr3);
    }
    // Children go to init.
    let orphans: Vec<Arc<Process>> = core::mem::take(&mut *p.children.lock());
    if !orphans.is_empty() {
        let init = by_pid(1).expect("no init");
        for c in &orphans {
            c.ppid.store(1, Ordering::Release);
        }
        init.children.lock().extend(orphans);
        init.child_wq.wake_all();
    }
    p.wstatus.store(wstatus, Ordering::Release);
    p.state.store(ZOMBIE, Ordering::Release);
    if let Some(parent) = by_pid(p.ppid.load(Ordering::Acquire)) {
        crate::signal::send(&parent, crate::signal::SIGCHLD);
        parent.child_wq.wake_all();
    }
    *p.task.lock() = None;
    sched::current().process.lock().take();
    drop(p);
    sched::exit_current();
}

pub const WNOHANG: u64 = 1;

/// wait4(): reaps a zombie child (pid -1: any; pid 0 / < -1: a process group).
pub fn wait(pid: i64, options: u64) -> KResult<(u32, i32)> {
    let me = current();
    let mut result: KResult<Option<(u32, i32)>> = Ok(None);
    let ok = me.child_wq.wait_until(|| {
        let mut kids = me.children.lock();
        let my_pgid = me.pgid.load(Ordering::Relaxed);
        let matches = |c: &Arc<Process>| match pid {
            -1 => true,
            0 => c.pgid.load(Ordering::Relaxed) == my_pgid,
            p if p < -1 => c.pgid.load(Ordering::Relaxed) as i64 == -p,
            p => c.pid as i64 == p,
        };
        if !kids.iter().any(&matches) {
            result = Err(ECHILD);
            return true;
        }
        if let Some(i) = kids.iter().position(|c| matches(c) && c.state.load(Ordering::Acquire) == ZOMBIE) {
            let c = kids.remove(i);
            TABLE.lock().remove(&c.pid);
            result = Ok(Some((c.pid, c.wstatus.load(Ordering::Acquire))));
            return true;
        }
        options & WNOHANG != 0
    });
    if !ok {
        return Err(EINTR);
    }
    result.map(|r| r.unwrap_or((0, 0)))
}
