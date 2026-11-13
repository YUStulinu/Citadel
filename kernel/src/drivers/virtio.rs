//! Virtio 1.x over PCI (the "modern" interface): capability-located configuration structures,
//! feature negotiation, split virtqueues and MSI-X interrupts. Used by the block and network
//! drivers.
use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering};

use super::pci::PciDev;
use crate::boot::{phys_to_virt, virt_to_phys};
use crate::mm::{frame, paging};

pub const VENDOR: u16 = 0x1af4;
pub const F_VERSION_1: u64 = 1 << 32;
/// used_event / avail_event: each side tells the other when it next needs to be notified.
pub const F_EVENT_IDX: u64 = 1 << 29;

const ACKNOWLEDGE: u8 = 1;
const DRIVER: u8 = 2;
const DRIVER_OK: u8 = 4;
const FEATURES_OK: u8 = 8;

pub const DESC_NEXT: u16 = 1;
pub const DESC_WRITE: u16 = 2;

/// Virtio device type from the PCI device id (modern 0x1040 + type, transitional 0x1000..).
pub fn device_type(d: &PciDev) -> Option<u16> {
    if d.vendor != VENDOR {
        return None;
    }
    match d.device {
        0x1040..=0x107f => Some(d.device - 0x1040),
        0x1000 => Some(1), // network
        0x1001 => Some(2), // block
        _ => None,
    }
}

pub struct Device {
    pub pci: PciDev,
    common: u64,
    notify: u64,
    notify_mult: u32,
    pub device_cfg: u64,
}

// Offsets in the common configuration structure.
const DEV_FEATURE_SEL: u64 = 0x00;
const DEV_FEATURE: u64 = 0x04;
const DRV_FEATURE_SEL: u64 = 0x08;
const DRV_FEATURE: u64 = 0x0c;
const MSIX_CONFIG: u64 = 0x10;
const STATUS: u64 = 0x14;
const Q_SELECT: u64 = 0x16;
const Q_SIZE: u64 = 0x18;
const Q_MSIX: u64 = 0x1a;
const Q_ENABLE: u64 = 0x1c;
const Q_NOTIFY_OFF: u64 = 0x1e;
const Q_DESC: u64 = 0x20;
const Q_DRIVER: u64 = 0x28;
const Q_DEVICE: u64 = 0x30;

fn r8(a: u64) -> u8 {
    unsafe { core::ptr::read_volatile(a as *const u8) }
}
fn r16(a: u64) -> u16 {
    unsafe { core::ptr::read_volatile(a as *const u16) }
}
fn r32(a: u64) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}
fn w8(a: u64, v: u8) {
    unsafe { core::ptr::write_volatile(a as *mut u8, v) }
}
fn w16(a: u64, v: u16) {
    unsafe { core::ptr::write_volatile(a as *mut u16, v) }
}
fn w32(a: u64, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}
fn w64(a: u64, v: u64) {
    w32(a, v as u32);
    w32(a + 4, (v >> 32) as u32);
}

impl Device {
    /// Finds the configuration structures, resets the device and negotiates `wanted` features
    /// (VERSION_1 is always requested). Returns the device and the accepted features.
    pub fn init(pci: PciDev, wanted: u64) -> Option<(Device, u64)> {
        pci.enable();
        let mut common = 0;
        let mut notify = 0;
        let mut notify_mult = 0;
        let mut device_cfg = 0;
        for cap in pci.capabilities(0x09) {
            let cfg_type = pci.read8(cap + 3);
            let bar = pci.read8(cap + 4);
            let offset = pci.read32(cap + 8) as u64;
            let length = pci.read32(cap + 12) as u64;
            let base = pci.bar(bar);
            if base == 0 || length == 0 {
                continue;
            }
            let virt = paging::map_mmio(base + offset, length);
            match cfg_type {
                1 if common == 0 => common = virt,
                2 if notify == 0 => {
                    notify = virt;
                    notify_mult = pci.read32(cap + 16);
                }
                4 if device_cfg == 0 => device_cfg = virt,
                _ => {}
            }
        }
        if common == 0 || notify == 0 {
            return None;
        }
        let d = Device { pci, common, notify, notify_mult, device_cfg };
        w8(d.common + STATUS, 0);
        while r8(d.common + STATUS) != 0 {
            core::hint::spin_loop();
        }
        w8(d.common + STATUS, ACKNOWLEDGE);
        w8(d.common + STATUS, ACKNOWLEDGE | DRIVER);
        let offered = d.features();
        let accepted = offered & (wanted | F_VERSION_1);
        w32(d.common + DRV_FEATURE_SEL, 0);
        w32(d.common + DRV_FEATURE, accepted as u32);
        w32(d.common + DRV_FEATURE_SEL, 1);
        w32(d.common + DRV_FEATURE, (accepted >> 32) as u32);
        w8(d.common + STATUS, ACKNOWLEDGE | DRIVER | FEATURES_OK);
        if r8(d.common + STATUS) & FEATURES_OK == 0 || accepted & F_VERSION_1 == 0 {
            crate::kprintln!("virtio: device refused features {accepted:#x}");
            return None;
        }
        w16(d.common + MSIX_CONFIG, 0xffff);
        Some((d, accepted))
    }

    fn features(&self) -> u64 {
        w32(self.common + DEV_FEATURE_SEL, 0);
        let lo = r32(self.common + DEV_FEATURE) as u64;
        w32(self.common + DEV_FEATURE_SEL, 1);
        let hi = r32(self.common + DEV_FEATURE) as u64;
        lo | hi << 32
    }

    /// Sets up queue `idx` with at most `max` entries, its interrupt on MSI-X entry `msix`.
    pub fn queue(&self, idx: u16, max: u16, msix: u16) -> Option<Queue> {
        w16(self.common + Q_SELECT, idx);
        let dev_size = r16(self.common + Q_SIZE);
        if dev_size == 0 {
            return None;
        }
        let size = dev_size.min(max).next_power_of_two().min(dev_size);
        w16(self.common + Q_SIZE, size);
        let q = Queue::new(size)?;
        w64(self.common + Q_DESC, q.desc_phys);
        w64(self.common + Q_DRIVER, q.avail_phys);
        w64(self.common + Q_DEVICE, q.used_phys);
        w16(self.common + Q_MSIX, msix);
        let off = r16(self.common + Q_NOTIFY_OFF) as u64;
        let mut q = q;
        q.notify_addr = self.notify + off * self.notify_mult as u64;
        q.index = idx;
        w16(self.common + Q_ENABLE, 1);
        Some(q)
    }

    pub fn driver_ok(&self) {
        let s = r8(self.common + STATUS);
        w8(self.common + STATUS, s | DRIVER_OK);
    }

    pub fn cfg_read32(&self, off: u64) -> u32 {
        r32(self.device_cfg + off)
    }
    pub fn cfg_read8(&self, off: u64) -> u8 {
        r8(self.device_cfg + off)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Desc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

/// A split virtqueue. Descriptors are handed out from a free list; a request is a chain.
pub struct Queue {
    pub size: u16,
    pub index: u16,
    desc: *mut Desc,
    avail: u64, // flags u16, idx u16, ring[size] u16
    used: u64,  // flags u16, idx u16, ring[size] {id u32, len u32}
    desc_phys: u64,
    avail_phys: u64,
    used_phys: u64,
    free: Vec<u16>,
    last_used: u16,
    notify_addr: u64,
    /// Negotiated F_EVENT_IDX (set by the driver after `queue`).
    pub event_idx: bool,
    kicked_at: u16,
}

unsafe impl Send for Queue {}

impl Queue {
    fn new(size: u16) -> Option<Queue> {
        let n = size as u64;
        let desc_bytes = 16 * n;
        let avail_bytes = 6 + 2 * n;
        let used_off = (desc_bytes + avail_bytes + 4095) & !4095;
        let total = used_off + 6 + 8 * n;
        let order = (total.div_ceil(4096)).next_power_of_two().trailing_zeros() as usize;
        let phys = frame::alloc_zeroed(order)?;
        let virt = phys_to_virt(phys);
        Some(Queue {
            size,
            index: 0,
            desc: virt as *mut Desc,
            avail: virt + desc_bytes,
            used: virt + used_off,
            desc_phys: phys,
            avail_phys: phys + desc_bytes,
            used_phys: phys + used_off,
            free: (0..size).rev().collect(),
            last_used: 0,
            notify_addr: 0,
            event_idx: false,
            kicked_at: 0,
        })
    }

    pub fn free_count(&self) -> usize {
        self.free.len()
    }

    /// Queues a chain of buffers (kernel virtual address, length, device-writable) and returns
    /// the head descriptor's index, or None when there are not enough free descriptors.
    pub fn submit(&mut self, bufs: &[(u64, u32, bool)]) -> Option<u16> {
        if bufs.is_empty() || self.free.len() < bufs.len() {
            return None;
        }
        let ids: Vec<u16> = (0..bufs.len()).map(|_| self.free.pop().unwrap()).collect();
        for (i, &(addr, len, write)) in bufs.iter().enumerate() {
            let mut flags = if write { DESC_WRITE } else { 0 };
            let next = if i + 1 < bufs.len() {
                flags |= DESC_NEXT;
                ids[i + 1]
            } else {
                0
            };
            unsafe { *self.desc.add(ids[i] as usize) = Desc { addr: virt_to_phys(addr), len, flags, next } };
        }
        let head = ids[0];
        let idx = r16(self.avail + 2);
        w16(self.avail + 4 + 2 * (idx % self.size) as u64, head);
        fence(Ordering::SeqCst);
        w16(self.avail + 2, idx.wrapping_add(1));
        fence(Ordering::SeqCst);
        Some(head)
    }

    /// Tells the device about new buffers, unless it said it does not need to know yet.
    pub fn notify(&mut self) {
        let new = r16(self.avail + 2);
        if self.event_idx {
            fence(Ordering::SeqCst);
            let old = self.kicked_at;
            let ev = r16(self.used + 4 + 8 * self.size as u64); // avail_event
            self.kicked_at = new;
            // vring_need_event(event, new, old)
            if new.wrapping_sub(ev).wrapping_sub(1) >= new.wrapping_sub(old) {
                return;
            }
        }
        w16(self.notify_addr, self.index);
    }

    /// Interrupts for used buffers: off (the driver polls) or on again. Returns whether there
    /// is already something to collect after turning them on.
    pub fn set_interrupts(&mut self, on: bool) -> bool {
        let used_event = self.avail + 4 + 2 * self.size as u64;
        if self.event_idx {
            w16(used_event, if on { self.last_used } else { self.last_used.wrapping_add(0x7fff) });
        } else {
            w16(self.avail, if on { 0 } else { 1 }); // VRING_AVAIL_F_NO_INTERRUPT
        }
        fence(Ordering::SeqCst);
        on && r16(self.used + 2) != self.last_used
    }

    /// Takes the next completed chain: (head index, bytes written by the device). Its
    /// descriptors return to the free list.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        fence(Ordering::SeqCst);
        let used_idx = r16(self.used + 2);
        if used_idx == self.last_used {
            return None;
        }
        let slot = self.used + 4 + 8 * (self.last_used % self.size) as u64;
        let id = r32(slot) as u16;
        let len = r32(slot + 4);
        self.last_used = self.last_used.wrapping_add(1);
        let mut d = id;
        loop {
            let desc = unsafe { *self.desc.add(d as usize) };
            self.free.push(d);
            if desc.flags & DESC_NEXT == 0 {
                break;
            }
            d = desc.next;
        }
        Some((id, len))
    }
}
