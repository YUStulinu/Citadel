//! PCI: configuration space through ports 0xCF8/0xCFC, enumeration of bus 0 and the buses behind
//! bridges, BARs, capabilities and MSI-X.
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::arch::port;
use crate::sync::SpinLock;

static CONFIG: SpinLock<()> = SpinLock::new(());

#[derive(Clone, Copy, Debug)]
pub struct PciDev {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
}

fn addr(d: &PciDev, off: u8) -> u32 {
    0x8000_0000 | (d.bus as u32) << 16 | (d.dev as u32) << 11 | (d.func as u32) << 8 | (off as u32 & 0xfc)
}

fn read_raw(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    let d = PciDev { bus, dev, func, vendor: 0, device: 0, class: 0, subclass: 0 };
    let _g = CONFIG.lock();
    port::outl(0xcf8, addr(&d, off));
    port::inl(0xcfc)
}

impl PciDev {
    pub fn read32(&self, off: u8) -> u32 {
        let _g = CONFIG.lock();
        port::outl(0xcf8, addr(self, off));
        port::inl(0xcfc)
    }
    pub fn write32(&self, off: u8, v: u32) {
        let _g = CONFIG.lock();
        port::outl(0xcf8, addr(self, off));
        port::outl(0xcfc, v);
    }
    pub fn read16(&self, off: u8) -> u16 {
        (self.read32(off) >> ((off & 2) * 8)) as u16
    }
    pub fn read8(&self, off: u8) -> u8 {
        (self.read32(off) >> ((off & 3) * 8)) as u8
    }
    pub fn write16(&self, off: u8, v: u16) {
        let shift = (off & 2) * 8;
        let old = self.read32(off);
        self.write32(off, (old & !(0xffff << shift)) | (v as u32) << shift);
    }

    /// Turns on memory decoding and bus mastering (DMA).
    pub fn enable(&self) {
        let cmd = self.read16(4);
        self.write16(4, cmd | 0x6);
    }

    /// Physical address of a memory BAR (32- or 64-bit).
    pub fn bar(&self, i: u8) -> u64 {
        let lo = self.read32(0x10 + 4 * i);
        if lo & 1 != 0 {
            return (lo & !3) as u64; // I/O space
        }
        let base = (lo & !0xf) as u64;
        if (lo >> 1) & 3 == 2 {
            base | (self.read32(0x14 + 4 * i) as u64) << 32
        } else {
            base
        }
    }

    /// Offsets of the capabilities with the given id.
    pub fn capabilities(&self, id: u8) -> Vec<u8> {
        let mut out = Vec::new();
        if self.read16(6) & (1 << 4) == 0 {
            return out;
        }
        let mut p = self.read8(0x34) & !3;
        let mut guard = 0;
        while p != 0 && guard < 48 {
            if self.read8(p) == id {
                out.push(p);
            }
            p = self.read8(p + 1) & !3;
            guard += 1;
        }
        out
    }

    /// Programs MSI-X table entry `entry` to deliver `vector` to the boot CPU and enables MSI-X.
    pub fn msix_route(&self, entry: u16, vector: u8) -> bool {
        let Some(&cap) = self.capabilities(0x11).first() else { return false };
        let ctrl = self.read16(cap + 2);
        let size = (ctrl & 0x7ff) + 1;
        if entry >= size {
            return false;
        }
        let tbl = self.read32(cap + 4);
        let bar = self.bar((tbl & 7) as u8);
        let table = crate::mm::paging::map_mmio(bar + (tbl & !7) as u64, size as u64 * 16);
        let dest = crate::arch::percpu::get(0).map(|p| p.apic_id).unwrap_or(0);
        unsafe {
            let e = (table + entry as u64 * 16) as *mut u32;
            core::ptr::write_volatile(e, 0xfee0_0000 | (dest << 12));
            core::ptr::write_volatile(e.add(1), 0);
            core::ptr::write_volatile(e.add(2), vector as u32);
            core::ptr::write_volatile(e.add(3), 0); // unmasked
        }
        // Enable MSI-X, clear the function mask; also disable legacy INTx.
        self.write16(cap + 2, (ctrl | (1 << 15)) & !(1 << 14));
        let cmd = self.read16(4);
        self.write16(4, cmd | (1 << 10));
        true
    }
}

static DEVICES: SpinLock<Vec<PciDev>> = SpinLock::new(Vec::new());

fn scan_bus(bus: u8, out: &mut Vec<PciDev>, depth: u32) {
    for dev in 0..32u8 {
        let id = read_raw(bus, dev, 0, 0);
        if id & 0xffff == 0xffff {
            continue;
        }
        let header = (read_raw(bus, dev, 0, 0x0c) >> 16) as u8;
        let funcs = if header & 0x80 != 0 { 8 } else { 1 };
        for func in 0..funcs {
            let id = read_raw(bus, dev, func, 0);
            if id & 0xffff == 0xffff {
                continue;
            }
            let class = read_raw(bus, dev, func, 8);
            let d = PciDev {
                bus,
                dev,
                func,
                vendor: id as u16,
                device: (id >> 16) as u16,
                class: (class >> 24) as u8,
                subclass: (class >> 16) as u8,
            };
            out.push(d);
            // PCI-to-PCI bridge: scan the secondary bus.
            if d.class == 6 && d.subclass == 4 && depth < 8 {
                let secondary = (read_raw(bus, dev, func, 0x18) >> 8) as u8;
                if secondary != 0 && secondary != bus {
                    scan_bus(secondary, out, depth + 1);
                }
            }
        }
    }
}

pub fn init() {
    let mut v = Vec::new();
    scan_bus(0, &mut v, 0);
    for d in &v {
        crate::kprintln!("pci: {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}.{:02x}", d.bus, d.dev, d.func, d.vendor, d.device, d.class, d.subclass);
    }
    *DEVICES.lock() = v;
}

pub fn devices() -> Vec<PciDev> {
    DEVICES.lock().clone()
}

/// A free interrupt vector for a device (MSI/MSI-X).
pub fn alloc_vector() -> u8 {
    static NEXT: AtomicU8 = AtomicU8::new(0x50);
    let v = NEXT.fetch_add(1, Ordering::Relaxed);
    assert!(v < 0xe0, "out of interrupt vectors");
    v
}
