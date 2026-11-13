//! Block devices: the virtio-blk driver, GPT partitions, and the list of disks the file systems
//! mount from.
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kcore::errno::*;

use super::pci::{self, PciDev};
use super::virtio;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

pub const SECTOR: u64 = 512;

/// Random access to a disk or partition, in whole sectors.
pub trait BlockDevice: Send + Sync {
    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<()>;
    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<()>;
    fn size(&self) -> u64;
    fn flush(&self) -> KResult<()> {
        Ok(())
    }
}

#[repr(C)]
struct ReqHeader {
    kind: u32,
    reserved: u32,
    sector: u64,
}

const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;
const F_RO: u64 = 1 << 5;
const F_FLUSH: u64 = 1 << 9;
const MAX_CHUNK: usize = 64 * 1024;
const QUEUE_MAX: u16 = 128;

pub struct VirtioBlk {
    dev: virtio::Device,
    q: SpinLock<virtio::Queue>,
    wq: WaitQueue,
    pending: Vec<AtomicBool>,
    capacity: u64,
    read_only: bool,
    can_flush: bool,
    irq: bool,
    pub reads: AtomicU64,
    pub writes: AtomicU64,
}

impl VirtioBlk {
    fn probe(p: PciDev) -> Option<&'static VirtioBlk> {
        let (dev, feat) = virtio::Device::init(p, F_RO | F_FLUSH)?;
        let vector = pci::alloc_vector();
        let irq = p.msix_route(0, vector);
        let q = dev.queue(0, QUEUE_MAX, if irq { 0 } else { 0xffff })?;
        let capacity = dev.cfg_read32(0) as u64 | (dev.cfg_read32(4) as u64) << 32;
        let size = q.size;
        let blk: &'static VirtioBlk = Box::leak(Box::new(VirtioBlk {
            dev,
            q: SpinLock::new(q),
            wq: WaitQueue::new(),
            pending: (0..size).map(|_| AtomicBool::new(false)).collect(),
            capacity,
            read_only: feat & F_RO != 0,
            can_flush: feat & F_FLUSH != 0,
            irq,
            reads: AtomicU64::new(0),
            writes: AtomicU64::new(0),
        }));
        if irq {
            crate::interrupts::register(vector, move || blk.complete());
        }
        blk.dev.driver_ok();
        crate::kprintln!(
            "virtio-blk: {} MiB{}{}",
            capacity * SECTOR >> 20,
            if blk.read_only { ", read-only" } else { "" },
            if irq { "" } else { ", polled" }
        );
        Some(blk)
    }

    /// Interrupt: mark finished requests and wake their waiters.
    fn complete(&self) {
        {
            let mut q = self.q.lock();
            while let Some((id, _)) = q.pop_used() {
                self.pending[id as usize].store(false, Ordering::Release);
            }
        }
        self.wq.wake_all();
    }

    fn request(&self, kind: u32, sector: u64, data: Option<(u64, u32, bool)>) -> KResult<()> {
        let hdr = Box::new(ReqHeader { kind, reserved: 0, sector });
        let status = Box::new(0xffu8);
        let hdr_addr = &*hdr as *const ReqHeader as u64;
        let st_addr = &*status as *const u8 as u64;
        let mut chain: Vec<(u64, u32, bool)> = alloc::vec![(hdr_addr, 16, false)];
        if let Some(d) = data {
            chain.push(d);
        }
        chain.push((st_addr, 1, true));
        // Submit (waiting for free descriptors if the queue is full).
        let head = loop {
            let mut q = self.q.lock();
            if let Some(h) = q.submit(&chain) {
                self.pending[h as usize].store(true, Ordering::Release);
                q.notify();
                break h;
            }
            drop(q);
            if crate::arch::cpu::irq_enabled() {
                self.wq.wait_until(|| self.q.lock().free_count() >= chain.len());
            } else {
                self.complete();
            }
        };
        // Sleep for the interrupt, except during boot (interrupts still off): then poll.
        if self.irq && crate::arch::cpu::irq_enabled() {
            while !self.wq.wait_until(|| !self.pending[head as usize].load(Ordering::Acquire)) {}
        } else {
            while self.pending[head as usize].load(Ordering::Acquire) {
                self.complete();
                core::hint::spin_loop();
            }
        }
        let st = unsafe { core::ptr::read_volatile(st_addr as *const u8) };
        drop(hdr);
        drop(status);
        if st == 0 { Ok(()) } else { Err(EIO) }
    }
}

impl BlockDevice for VirtioBlk {
    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        if off % SECTOR != 0 || buf.len() as u64 % SECTOR != 0 || off + buf.len() as u64 > self.size() {
            return Err(EINVAL);
        }
        for (i, chunk) in buf.chunks_mut(MAX_CHUNK).enumerate() {
            let sector = (off + (i * MAX_CHUNK) as u64) / SECTOR;
            self.request(T_IN, sector, Some((chunk.as_mut_ptr() as u64, chunk.len() as u32, true)))?;
        }
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        if off % SECTOR != 0 || buf.len() as u64 % SECTOR != 0 || off + buf.len() as u64 > self.size() {
            return Err(EINVAL);
        }
        for (i, chunk) in buf.chunks(MAX_CHUNK).enumerate() {
            let sector = (off + (i * MAX_CHUNK) as u64) / SECTOR;
            self.request(T_OUT, sector, Some((chunk.as_ptr() as u64, chunk.len() as u32, false)))?;
        }
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn size(&self) -> u64 {
        self.capacity * SECTOR
    }

    fn flush(&self) -> KResult<()> {
        if self.can_flush { self.request(T_FLUSH, 0, None) } else { Ok(()) }
    }
}

/// A slice of another device (a partition).
pub struct Partition {
    pub disk: &'static dyn BlockDevice,
    pub start: u64,
    pub len: u64,
}

impl BlockDevice for Partition {
    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        if off + buf.len() as u64 > self.len {
            return Err(EINVAL);
        }
        self.disk.read_at(self.start + off, buf)
    }
    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<()> {
        if off + buf.len() as u64 > self.len {
            return Err(EINVAL);
        }
        self.disk.write_at(self.start + off, buf)
    }
    fn size(&self) -> u64 {
        self.len
    }
    fn flush(&self) -> KResult<()> {
        self.disk.flush()
    }
}

pub struct Disk {
    pub name: String,
    pub dev: Arc<dyn BlockDevice>,
    /// GPT partition type GUID (as stored on disk), or zero for a whole disk.
    pub type_guid: [u8; 16],
    pub label: String,
}

static DISKS: SpinLock<Vec<Arc<Disk>>> = SpinLock::new(Vec::new());

pub fn disks() -> Vec<Arc<Disk>> {
    DISKS.lock().clone()
}

/// The "Linux filesystem data" GPT type 0FC63DAF-8483-4772-8E79-3D69D8477DE4, in on-disk order.
pub const LINUX_FS_GUID: [u8; 16] = [0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4];

fn read_gpt(name: &str, disk: &'static dyn BlockDevice) -> Vec<Arc<Disk>> {
    let mut out = Vec::new();
    let mut hdr = alloc::vec![0u8; 512];
    if disk.read_at(SECTOR, &mut hdr).is_err() || &hdr[0..8] != b"EFI PART" {
        return out;
    }
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let entries_lba = u64_at(&hdr, 72);
    let count = u32_at(&hdr, 80).min(128) as usize;
    let esize = u32_at(&hdr, 84) as usize;
    if esize < 128 || esize > 512 {
        return out;
    }
    let bytes = (count * esize).div_ceil(512) * 512;
    let mut tbl = alloc::vec![0u8; bytes];
    if disk.read_at(entries_lba * SECTOR, &mut tbl).is_err() {
        return out;
    }
    for i in 0..count {
        let e = &tbl[i * esize..(i + 1) * esize];
        let mut type_guid = [0u8; 16];
        type_guid.copy_from_slice(&e[0..16]);
        if type_guid == [0; 16] {
            continue;
        }
        let first = u64_at(e, 32);
        let last = u64_at(e, 40);
        let label: String = e[56..128].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).map(|c| char::from_u32(c as u32).unwrap_or('?')).collect();
        let part = Partition { disk, start: first * SECTOR, len: (last + 1 - first) * SECTOR };
        out.push(Arc::new(Disk { name: alloc::format!("{name}{}", i + 1), dev: Arc::new(part), type_guid, label }));
    }
    out
}

pub fn init() {
    let mut n = 0;
    for p in pci::devices() {
        if virtio::device_type(&p) != Some(2) {
            continue;
        }
        let Some(blk) = VirtioBlk::probe(p) else { continue };
        let name = alloc::format!("vd{}", (b'a' + n) as char);
        n += 1;
        let parts = read_gpt(&name, blk);
        for d in &parts {
            crate::kprintln!("disk: {} \"{}\" {} MiB", d.name, d.label, d.dev.size() >> 20);
        }
        let mut list = DISKS.lock();
        list.push(Arc::new(Disk { name, dev: Arc::new(StaticDev(blk)), type_guid: [0; 16], label: String::new() }));
        list.extend(parts);
    }
}

/// Adapts a &'static device to Arc<dyn BlockDevice>.
struct StaticDev(&'static VirtioBlk);

impl BlockDevice for StaticDev {
    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        self.0.read_at(off, buf)
    }
    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<()> {
        self.0.write_at(off, buf)
    }
    fn size(&self) -> u64 {
        self.0.size()
    }
    fn flush(&self) -> KResult<()> {
        self.0.flush()
    }
}
