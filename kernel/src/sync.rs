//! Spinlocks that also disable interrupts on the local CPU while held (an interrupt handler
//! taking a lock its own CPU already holds would otherwise deadlock).
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::cpu;

/// A ticket lock: CPUs get the lock in the order they asked for it.
pub struct SpinLock<T> {
    next: AtomicU32,
    serving: AtomicU32,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

pub struct Guard<'a, T> {
    lock: &'a SpinLock<T>,
    irq_was_on: bool,
}

impl<T> SpinLock<T> {
    pub const fn new(v: T) -> Self {
        SpinLock { next: AtomicU32::new(0), serving: AtomicU32::new(0), value: UnsafeCell::new(v) }
    }

    pub fn lock(&self) -> Guard<'_, T> {
        let irq_was_on = cpu::irq_save();
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        while self.serving.load(Ordering::Acquire) != ticket {
            core::hint::spin_loop();
        }
        Guard { lock: self, irq_was_on }
    }

    pub fn try_lock(&self) -> Option<Guard<'_, T>> {
        let irq_was_on = cpu::irq_save();
        let s = self.serving.load(Ordering::Relaxed);
        if self.next.compare_exchange(s, s + 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            Some(Guard { lock: self, irq_was_on })
        } else {
            cpu::irq_restore(irq_was_on);
            None
        }
    }

    pub fn is_locked(&self) -> bool {
        self.next.load(Ordering::Relaxed) != self.serving.load(Ordering::Relaxed)
    }

    /// # Safety
    /// For the panic path only: takes the value without the lock.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn force(&self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }
}

impl<T> Guard<'_, T> {
    /// Releases the lock but leaves interrupts disabled (the scheduler switches tasks with
    /// interrupts off, after dropping the run-queue lock).
    pub fn into_irq_state(self) -> bool {
        let on = self.irq_was_on;
        self.lock.serving.fetch_add(1, Ordering::Release);
        core::mem::forget(self);
        on
    }
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.lock.serving.fetch_add(1, Ordering::Release);
        cpu::irq_restore(self.irq_was_on);
    }
}

/// A value initialised once at boot, read without locking afterwards.
pub struct Once<T> {
    state: AtomicU32,
    value: UnsafeCell<core::mem::MaybeUninit<T>>,
}

unsafe impl<T: Send + Sync> Sync for Once<T> {}

impl<T> Once<T> {
    pub const fn new() -> Self {
        Once { state: AtomicU32::new(0), value: UnsafeCell::new(core::mem::MaybeUninit::uninit()) }
    }
    pub fn set(&self, v: T) {
        if self.state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
            panic!("Once set twice");
        }
        unsafe { (*self.value.get()).write(v) };
        self.state.store(2, Ordering::Release);
    }
    pub fn get(&self) -> &T {
        assert!(self.state.load(Ordering::Acquire) == 2, "Once used before it was set");
        unsafe { (*self.value.get()).assume_init_ref() }
    }
    pub fn try_get(&self) -> Option<&T> {
        if self.state.load(Ordering::Acquire) == 2 { Some(unsafe { (*self.value.get()).assume_init_ref() }) } else { None }
    }
}

impl<T> Default for Once<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A lock whose waiters sleep (for long critical sections that do disk I/O, like a file
/// system's metadata updates). Interrupts stay enabled while it is held.
pub struct Mutex<T> {
    locked: core::sync::atomic::AtomicBool,
    wq: crate::sched::WaitQueue,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

pub struct MutexGuard<'a, T> {
    m: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(v: T) -> Self {
        Mutex { locked: core::sync::atomic::AtomicBool::new(false), wq: crate::sched::WaitQueue::new(), value: UnsafeCell::new(v) }
    }

    fn try_take(&self) -> bool {
        self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        // Signals do not interrupt this wait: callers expect to get the lock.
        while !self.wq.wait_until(|| self.try_take()) {}
        MutexGuard { m: self }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.m.locked.store(false, Ordering::Release);
        self.m.wq.wake_one();
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.m.value.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.m.value.get() }
    }
}
