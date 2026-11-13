//! 16550 UART on COM1: the kernel log, and the console tty's hardware. COM2 carries the
//! profiler's samples to the host.
use crate::arch::port::{inb, outb};
use crate::sync::SpinLock;
use core::fmt::{self, Write};

pub const COM1: u16 = 0x3f8;
pub const COM2: u16 = 0x2f8;

pub fn init_port(base: u16) {
    outb(base + 1, 0x00); // no interrupts yet
    outb(base + 3, 0x80); // DLAB
    outb(base, 0x01); // divisor 1: 115200 baud
    outb(base + 1, 0x00);
    outb(base + 3, 0x03); // 8N1
    outb(base + 2, 0xc7); // FIFO on, cleared, 14-byte threshold
    outb(base + 4, 0x0b); // RTS/DSR, OUT2 (needed for IRQs)
}

pub fn putb(base: u16, c: u8) {
    let mut spins = 0u32;
    while inb(base + 5) & 0x20 == 0 && spins < 1_000_000 {
        spins += 1;
        core::hint::spin_loop();
    }
    outb(base, c);
}

/// A byte waiting in the receive buffer, if any.
pub fn try_getb(base: u16) -> Option<u8> {
    if inb(base + 5) & 1 != 0 { Some(inb(base)) } else { None }
}

/// Enables the "data received" interrupt.
pub fn enable_rx_irq(base: u16) {
    outb(base + 1, 0x01);
}

pub struct Port(pub u16);

impl Write for Port {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                putb(self.0, b'\r');
            }
            putb(self.0, b);
        }
        Ok(())
    }
}

static LOG: SpinLock<()> = SpinLock::new(());

pub fn print(args: fmt::Arguments) {
    let _g = LOG.lock();
    let _ = Port(COM1).write_fmt(args);
}

/// For panics: no locks (the lock holder may be the one that panicked).
pub fn print_raw(args: fmt::Arguments) {
    let _ = Port(COM1).write_fmt(args);
}

#[macro_export]
macro_rules! kprint {
    ($($a:tt)*) => { $crate::serial::print(format_args!($($a)*)) };
}

#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($a:tt)*) => { $crate::serial::print(format_args!("{}\n", format_args!($($a)*))) };
}

/// Holds the console output lock (so a program's write is not interleaved with kernel logs).
pub fn lock_output() -> crate::sync::Guard<'static, ()> {
    LOG.lock()
}
