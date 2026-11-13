//! The scheduler: preemptive round robin with one run queue per CPU, work stealing between
//! CPUs, sleeping on wait queues, and timed sleeps.
//!
//! A task being switched away from stays "on its CPU" until the switch has finished saving its
//! stack pointer; another CPU that picks it from a run queue waits for that, so a task never runs
//! on two CPUs at once. Wake-ups go through a compare-and-swap of the task's state (Sleeping to
//! Runnable), so a wake-up that races with going to sleep is never lost and a task is never
//! queued twice.
use alloc::boxed::Box;
use alloc::collections::{BinaryHeap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::cell::UnsafeCell;
use core::cmp::Reverse;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::arch::{apic, cpu, percpu};
use crate::mm::frame;
use crate::boot::phys_to_virt;
use crate::sync::SpinLock;

pub const RUNNING: u8 = 0;
pub const RUNNABLE: u8 = 1;
pub const SLEEPING: u8 = 2;
pub const DEAD: u8 = 3;

pub const KSTACK_ORDER: usize = 2; // 16 KiB kernel stacks
pub const HZ: u64 = 250;
const SLICE_TICKS: u32 = 5; // 20 ms

#[repr(C, align(16))]
pub struct FpuState(pub [u8; 512]);

pub struct Task {
    pub tid: u32,
    pub name: SpinLock<String>,
    kstack_phys: u64,
    ctx_rsp: UnsafeCell<u64>,
    pub state: AtomicU8,
    on_cpu: AtomicBool,
    pub cpu: AtomicUsize,
    entry: SpinLock<Option<Box<dyn FnOnce() + Send>>>,
    pub fpu: UnsafeCell<FpuState>,
    pub fs_base: AtomicU64,
    /// The process this thread belongs to (None for kernel threads).
    pub process: SpinLock<Option<Arc<crate::proc::Process>>>,
    pub cr3: AtomicU64,
    pub runtime_ns: AtomicU64,
    switched_in_at: AtomicU64,
    pub switches: AtomicU64,
    /// Set when a signal should interrupt a sleep.
    pub interrupted: AtomicBool,
    pub is_idle: bool,
}

unsafe impl Sync for Task {}
unsafe impl Send for Task {}

impl Task {
    pub fn kstack_top(&self) -> u64 {
        phys_to_virt(self.kstack_phys) + (4096 << KSTACK_ORDER)
    }
    pub fn is_dead(&self) -> bool {
        self.state.load(Ordering::Acquire) == DEAD
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if self.kstack_phys != 0 {
            frame::free(self.kstack_phys, KSTACK_ORDER);
        }
    }
}

static NEXT_TID: AtomicU32 = AtomicU32::new(1);

pub struct CpuSched {
    rq: SpinLock<VecDeque<Arc<Task>>>,
    current: UnsafeCell<Option<Arc<Task>>>,
    idle: UnsafeCell<Option<Arc<Task>>>,
    prev: UnsafeCell<Option<Arc<Task>>>,
    slice: AtomicU32,
    pub need_resched: AtomicBool,
    pub switches: AtomicU64,
    pub idle_ns: AtomicU64,
}

impl CpuSched {
    pub const fn new() -> Self {
        CpuSched {
            rq: SpinLock::new(VecDeque::new()),
            current: UnsafeCell::new(None),
            idle: UnsafeCell::new(None),
            prev: UnsafeCell::new(None),
            slice: AtomicU32::new(SLICE_TICKS),
            need_resched: AtomicBool::new(false),
            switches: AtomicU64::new(0),
            idle_ns: AtomicU64::new(0),
        }
    }
    pub fn queued(&self) -> usize {
        self.rq.lock().len()
    }
}

impl Default for CpuSched {
    fn default() -> Self {
        Self::new()
    }
}

// switch_context(save_rsp: *mut u64, new_rsp: u64): saves the callee-saved registers on the
// current stack, stores rsp, loads the other task's rsp and pops its registers.
global_asm!(
    r#"
    .global switch_context
switch_context:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret
"#
);

unsafe extern "C" {
    fn switch_context(save: *mut u64, new: u64);
}

/// Where a new task starts: finish the switch, enable interrupts, run its function.
extern "C" fn task_start() -> ! {
    finish_switch();
    cpu::enable_irq();
    let t = current();
    let f = t.entry.lock().take();
    if let Some(f) = f {
        f();
    }
    exit_current();
}

fn new_task(name: &str, f: Option<Box<dyn FnOnce() + Send>>, idle: bool) -> Arc<Task> {
    let kstack = frame::alloc_zeroed(KSTACK_ORDER).expect("out of memory for a kernel stack");
    let t = Arc::new(Task {
        tid: NEXT_TID.fetch_add(1, Ordering::Relaxed),
        name: SpinLock::new(String::from(name)),
        kstack_phys: kstack,
        ctx_rsp: UnsafeCell::new(0),
        state: AtomicU8::new(RUNNABLE),
        on_cpu: AtomicBool::new(false),
        cpu: AtomicUsize::new(0),
        entry: SpinLock::new(f),
        fpu: UnsafeCell::new(FpuState([0; 512])),
        fs_base: AtomicU64::new(0),
        process: SpinLock::new(None),
        cr3: AtomicU64::new(crate::mm::paging::kernel_pml4()),
        runtime_ns: AtomicU64::new(0),
        switched_in_at: AtomicU64::new(0),
        switches: AtomicU64::new(0),
        interrupted: AtomicBool::new(false),
        is_idle: idle,
    });
    // Initial FPU state: what fninit + default MXCSR give.
    unsafe {
        let fpu = &mut (*t.fpu.get()).0;
        fpu[0..2].copy_from_slice(&0x037fu16.to_le_bytes()); // FCW
        fpu[24..28].copy_from_slice(&0x1f80u32.to_le_bytes()); // MXCSR
    }
    // Initial stack: six zeroed callee-saved registers, then task_start as the return address.
    let top = t.kstack_top();
    let frame = top - 8 * 8;
    unsafe {
        let s = frame as *mut u64;
        for i in 0..6 {
            *s.add(i) = 0;
        }
        *s.add(6) = task_start as *const () as usize as u64;
        *s.add(7) = 0;
        *t.ctx_rsp.get() = frame;
    }
    t
}

/// Creates a kernel thread (not started).
pub fn spawn_kernel(name: &str, f: impl FnOnce() + Send + 'static) -> Arc<Task> {
    let t = new_task(name, Some(Box::new(f)), false);
    enqueue(t.clone());
    t
}

/// A task that will run `f` (used by fork and the first user process); not queued yet.
pub fn create_task(name: &str, f: impl FnOnce() + Send + 'static) -> Arc<Task> {
    new_task(name, Some(Box::new(f)), false)
}

pub fn current() -> Arc<Task> {
    unsafe { (*percpu::this().sched.current.get()).clone().expect("no current task") }
}

pub fn try_current() -> Option<Arc<Task>> {
    unsafe { (*percpu::this().sched.current.get()).clone() }
}

/// Becomes the scheduler on this CPU: the boot code continues as this CPU's idle task.
pub fn init_cpu() {
    let me = new_task("idle", None, true);
    me.state.store(RUNNING, Ordering::Release);
    me.on_cpu.store(true, Ordering::Release);
    me.cpu.store(percpu::cpu_id(), Ordering::Release);
    me.switched_in_at.store(apic::now_ns(), Ordering::Relaxed);
    let cs = &percpu::this().sched;
    unsafe {
        *cs.idle.get() = Some(me.clone());
        *cs.current.get() = Some(me);
    }
}

/// Picks a CPU for a runnable task: its last CPU if online, else the least loaded.
fn pick_cpu(t: &Task) -> usize {
    let n = percpu::online();
    let last = t.cpu.load(Ordering::Relaxed);
    let mut best = last.min(n - 1);
    let mut best_len = usize::MAX;
    for i in 0..n {
        if let Some(pc) = percpu::get(i) {
            let len = pc.sched.queued() + if unsafe { (*pc.sched.current.get()).as_ref().is_some_and(|c| !c.is_idle) } { 1 } else { 0 };
            if len < best_len || (len == best_len && i == last) {
                best = i;
                best_len = len;
            }
        }
    }
    best
}

pub fn enqueue(t: Arc<Task>) {
    let cpu_id = pick_cpu(&t);
    let pc = percpu::get(cpu_id).unwrap();
    t.state.store(RUNNABLE, Ordering::Release);
    pc.sched.rq.lock().push_back(t);
    if cpu_id != percpu::cpu_id() {
        apic::ipi(pc.apic_id, apic::IPI_RESCHEDULE);
    } else {
        pc.sched.need_resched.store(true, Ordering::Release);
    }
}

/// Wakes a sleeping task (no effect on a task that is not asleep).
pub fn wake(t: &Arc<Task>) {
    if t.state.compare_exchange(SLEEPING, RUNNABLE, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        enqueue(t.clone());
    }
}

/// Takes a task from the busiest other CPU (when ours has nothing to do).
fn steal() -> Option<Arc<Task>> {
    let me = percpu::cpu_id();
    let mut best: Option<(usize, usize)> = None;
    for i in 0..percpu::online() {
        if i == me {
            continue;
        }
        if let Some(pc) = percpu::get(i) {
            let n = pc.sched.queued();
            if n > 0 && best.is_none_or(|(_, b)| n > b) {
                best = Some((i, n));
            }
        }
    }
    let (i, _) = best?;
    percpu::get(i)?.sched.rq.try_lock()?.pop_back()
}

fn finish_switch() {
    let cs = &percpu::this().sched;
    if let Some(prev) = unsafe { (*cs.prev.get()).take() } {
        prev.on_cpu.store(false, Ordering::Release);
    }
}

/// Gives up the CPU. If the current task is still RUNNING it goes back to the run queue;
/// a task that set itself SLEEPING or DEAD does not.
pub fn schedule() {
    let irq = cpu::irq_save();
    let pc = percpu::this();
    let cs = &pc.sched;
    let cur = unsafe { (*cs.current.get()).clone().unwrap() };
    cs.need_resched.store(false, Ordering::Relaxed);
    cs.slice.store(SLICE_TICKS, Ordering::Relaxed);
    let next = {
        let mut rq = cs.rq.lock();
        if cur.state.load(Ordering::Acquire) == RUNNING && !cur.is_idle {
            cur.state.store(RUNNABLE, Ordering::Release);
            rq.push_back(cur.clone());
        }
        let n = rq.pop_front();
        drop(rq);
        n.or_else(steal).unwrap_or_else(|| unsafe { (*cs.idle.get()).clone().unwrap() })
    };
    if Arc::ptr_eq(&next, &cur) {
        cur.state.store(RUNNING, Ordering::Release);
        cpu::irq_restore(irq);
        return;
    }
    // The task may still be finishing a switch on another CPU.
    while next.on_cpu.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    next.on_cpu.store(true, Ordering::Release);
    next.state.store(RUNNING, Ordering::Release);
    next.cpu.store(pc.id, Ordering::Release);
    next.switches.fetch_add(1, Ordering::Relaxed);
    cs.switches.fetch_add(1, Ordering::Relaxed);
    let now = apic::now_ns();
    let ran = now.saturating_sub(cur.switched_in_at.load(Ordering::Relaxed));
    cur.runtime_ns.fetch_add(ran, Ordering::Relaxed);
    if cur.is_idle {
        cs.idle_ns.fetch_add(ran, Ordering::Relaxed);
    }
    next.switched_in_at.store(now, Ordering::Relaxed);

    // Hardware state: FPU/SSE registers, FS base (TLS), address space, kernel stack.
    unsafe {
        core::arch::asm!("fxsave64 [{}]", in(reg) (*cur.fpu.get()).0.as_mut_ptr());
        core::arch::asm!("fxrstor64 [{}]", in(reg) (*next.fpu.get()).0.as_ptr());
    }
    cur.fs_base.store(cpu::rdmsr(cpu::MSR_FS_BASE), Ordering::Relaxed);
    cpu::wrmsr(cpu::MSR_FS_BASE, next.fs_base.load(Ordering::Relaxed));
    let cr3 = next.cr3.load(Ordering::Acquire);
    if cpu::read_cr3() & crate::mm::paging::ADDR != cr3 {
        cpu::write_cr3(cr3);
    }
    percpu::set_kernel_stack(next.kstack_top());

    let save = cur.ctx_rsp.get();
    let new_rsp = unsafe { *next.ctx_rsp.get() };
    unsafe {
        *cs.prev.get() = Some(cur.clone());
        *cs.current.get() = Some(next);
    }
    drop(cur); // the reference in `prev` keeps the task alive until finish_switch
    unsafe { switch_context(save, new_rsp) };
    // Back on this task, possibly on another CPU.
    finish_switch();
    cpu::irq_restore(irq);
}

/// Called on every timer tick of this CPU.
pub fn tick() {
    let pc = percpu::this();
    pc.ticks.fetch_add(1, Ordering::Relaxed);
    if pc.id == 0 {
        fire_timers();
    }
    let s = pc.sched.slice.load(Ordering::Relaxed);
    if s <= 1 {
        pc.sched.need_resched.store(true, Ordering::Relaxed);
    } else {
        pc.sched.slice.store(s - 1, Ordering::Relaxed);
    }
    // An idle CPU looks for work.
    if unsafe { (*pc.sched.current.get()).as_ref().is_some_and(|c| c.is_idle) } && pc.sched.queued() == 0 {
        for i in 0..percpu::online() {
            if i != pc.id && percpu::get(i).is_some_and(|o| o.sched.queued() > 1) {
                pc.sched.need_resched.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// The idle loop: halt until an interrupt, then run whatever became runnable.
pub fn idle_loop() -> ! {
    loop {
        schedule();
        let pc = percpu::this();
        if pc.sched.queued() == 0 && !pc.sched.need_resched.load(Ordering::Relaxed) {
            cpu::enable_and_halt();
        }
    }
}

/// Ends the current task (its resources are freed once nothing refers to it).
pub fn exit_current() -> ! {
    current().state.store(DEAD, Ordering::Release);
    schedule();
    unreachable!("a dead task was scheduled");
}

/// Abandons a sleep that was announced (state SLEEPING) but not started. A waker may already
/// have made the task runnable and queued it; then it must give up the CPU once, so that the
/// queued entry is consumed by this very task instead of being left behind (a stale entry would
/// later run the task on two CPUs, or after it died).
fn cancel_sleep(me: &Arc<Task>) {
    if me.state.compare_exchange(SLEEPING, RUNNING, Ordering::AcqRel, Ordering::Acquire).is_err() {
        schedule();
    }
}

pub fn yield_now() {
    schedule();
}

// ---------------------------------------------------------------- sleeping

pub struct WaitQueue {
    waiters: SpinLock<Vec<Arc<Task>>>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        WaitQueue { waiters: SpinLock::new(Vec::new()) }
    }

    /// Sleeps until `cond()` is true. Returns false if interrupted by a signal first.
    pub fn wait_until(&self, mut cond: impl FnMut() -> bool) -> bool {
        let me = current();
        loop {
            me.state.store(SLEEPING, Ordering::Release);
            self.waiters.lock().push(me.clone());
            let done = cond();
            if done || me.interrupted.load(Ordering::Acquire) {
                self.remove(&me);
                cancel_sleep(&me);
                return done;
            }
            schedule();
            self.remove(&me);
        }
    }

    fn remove(&self, t: &Arc<Task>) {
        self.waiters.lock().retain(|w| !Arc::ptr_eq(w, t));
    }

    pub fn wake_all(&self) {
        let ws: Vec<Arc<Task>> = core::mem::take(&mut *self.waiters.lock());
        for w in ws {
            wake(&w);
        }
        // Anything that changes readiness also wakes the tasks in poll/select.
        if !core::ptr::eq(self, &POLL_WQ) {
            POLL_WQ.wake_all();
        }
    }

    pub fn wake_one(&self) {
        let w = {
            let mut q = self.waiters.lock();
            if q.is_empty() { None } else { Some(q.remove(0)) }
        };
        if let Some(w) = w {
            wake(&w);
        }
        POLL_WQ.wake_all();
    }
}

/// Tasks waiting in poll/select sleep here; every wait queue's wake-up wakes them too.
pub static POLL_WQ: WaitQueue = WaitQueue::new();

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

struct Timer {
    deadline: u64,
    task: Arc<Task>,
}
impl PartialEq for Timer {
    fn eq(&self, o: &Self) -> bool {
        self.deadline == o.deadline
    }
}
impl Eq for Timer {}
impl PartialOrd for Timer {
    fn partial_cmp(&self, o: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Timer {
    fn cmp(&self, o: &Self) -> core::cmp::Ordering {
        self.deadline.cmp(&o.deadline)
    }
}

static TIMERS: SpinLock<BinaryHeap<Reverse<Timer>>> = SpinLock::new(BinaryHeap::new());

fn fire_timers() {
    let now = apic::now_ns();
    loop {
        let t = {
            let mut h = TIMERS.lock();
            match h.peek() {
                Some(Reverse(t)) if t.deadline <= now => h.pop().map(|Reverse(t)| t.task),
                _ => None,
            }
        };
        match t {
            Some(task) => wake(&task),
            None => break,
        }
    }
}

/// Wakes the current task at `deadline` (TSC nanoseconds) if it is asleep then.
pub fn arm_timer(deadline: u64) {
    TIMERS.lock().push(Reverse(Timer { deadline, task: current() }));
}

/// Sleeps for at least `ns` nanoseconds (the timer runs at HZ, so the resolution is 4 ms; very
/// short sleeps spin). Returns false if interrupted by a signal.
pub fn sleep_ns(ns: u64) -> bool {
    let deadline = apic::now_ns() + ns;
    if ns < 1_000_000 / HZ * 1000 / 4 {
        while apic::now_ns() < deadline {
            core::hint::spin_loop();
        }
        return true;
    }
    let me = current();
    loop {
        if apic::now_ns() >= deadline {
            return true;
        }
        if me.interrupted.load(Ordering::Acquire) {
            return false;
        }
        me.state.store(SLEEPING, Ordering::Release);
        TIMERS.lock().push(Reverse(Timer { deadline, task: me.clone() }));
        schedule();
    }
}

/// Run-queue statistics per CPU: (queued, context switches, idle ns, ticks).
pub fn stats() -> Vec<(usize, u64, u64, u64)> {
    (0..percpu::online())
        .filter_map(percpu::get)
        .map(|pc| (pc.sched.queued(), pc.sched.switches.load(Ordering::Relaxed), pc.sched.idle_ns.load(Ordering::Relaxed), pc.ticks.load(Ordering::Relaxed)))
        .collect()
}
