//! virtio-net: one receive queue kept full of 2 KiB buffers, one transmit queue. Received
//! frames go to the network stack's input queue; the stack's thread does the protocol work.
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::pci::{self, PciDev};
use super::virtio;
use crate::sync::SpinLock;

const F_MAC: u64 = 1 << 5;
const HDR: usize = 12; // struct virtio_net_hdr with num_buffers (VERSION_1)
const RX_BUF: usize = 2048;
const QUEUE_MAX: u16 = 256;

pub struct VirtioNet {
    dev: virtio::Device,
    rx: SpinLock<RxState>,
    tx: SpinLock<TxState>,
    pub mac: [u8; 6],
    pub rx_frames: AtomicU64,
    pub tx_frames: AtomicU64,
    pub tx_dropped: AtomicU64,
}

struct RxState {
    q: virtio::Queue,
    bufs: Vec<Option<Box<[u8; RX_BUF]>>>, // by head descriptor
}

struct TxState {
    q: virtio::Queue,
    bufs: Vec<Option<Vec<u8>>>,
}

impl VirtioNet {
    fn probe(p: PciDev) -> Option<&'static VirtioNet> {
        let (dev, feat) = virtio::Device::init(p, F_MAC)?;
        let vector = pci::alloc_vector();
        let irq = p.msix_route(0, vector);
        let msix = if irq { 0 } else { 0xffff };
        let rxq = dev.queue(0, QUEUE_MAX, msix)?;
        let txq = dev.queue(1, QUEUE_MAX, msix)?;
        let mut mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        if feat & F_MAC != 0 {
            for (i, m) in mac.iter_mut().enumerate() {
                *m = dev.cfg_read8(i as u64);
            }
        }
        let (rs, ts) = (rxq.size as usize, txq.size as usize);
        let net: &'static VirtioNet = Box::leak(Box::new(VirtioNet {
            dev,
            rx: SpinLock::new(RxState { q: rxq, bufs: (0..rs).map(|_| None).collect() }),
            tx: SpinLock::new(TxState { q: txq, bufs: (0..ts).map(|_| None).collect() }),
            mac,
            rx_frames: AtomicU64::new(0),
            tx_frames: AtomicU64::new(0),
            tx_dropped: AtomicU64::new(0),
        }));
        {
            let mut rx = net.rx.lock();
            while rx.q.free_count() > 0 {
                let b = Box::new([0u8; RX_BUF]);
                let addr = b.as_ptr() as u64;
                let Some(h) = rx.q.submit(&[(addr, RX_BUF as u32, true)]) else { break };
                rx.bufs[h as usize] = Some(b);
            }
            rx.q.notify();
        }
        if irq {
            crate::interrupts::register(vector, move || net.interrupt());
        }
        net.dev.driver_ok();
        crate::kprintln!("virtio-net: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}{}", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], if irq { "" } else { " (no MSI-X)" });
        Some(net)
    }

    /// Interrupt: hand received frames to the stack, recycle their buffers, reap sent frames.
    fn interrupt(&self) {
        let mut frames = Vec::new();
        {
            let mut rx = self.rx.lock();
            while let Some((h, len)) = rx.q.pop_used() {
                if let Some(b) = rx.bufs[h as usize].take() {
                    let len = (len as usize).min(RX_BUF);
                    if len > HDR {
                        frames.push(b[HDR..len].to_vec());
                    }
                    let addr = b.as_ptr() as u64;
                    if let Some(nh) = rx.q.submit(&[(addr, RX_BUF as u32, true)]) {
                        rx.bufs[nh as usize] = Some(b);
                    }
                }
            }
            rx.q.notify();
        }
        self.reap_tx();
        if !frames.is_empty() {
            self.rx_frames.fetch_add(frames.len() as u64, Ordering::Relaxed);
            crate::net::receive(frames);
        }
    }

    fn reap_tx(&self) {
        let mut tx = self.tx.lock();
        while let Some((h, _)) = tx.q.pop_used() {
            tx.bufs[h as usize] = None;
        }
    }

    /// Queues an Ethernet frame for sending (dropped if the queue is full).
    pub fn send(&self, frame: &[u8]) {
        let mut buf = alloc::vec![0u8; HDR + frame.len()];
        buf[HDR..].copy_from_slice(frame);
        let mut tx = self.tx.lock();
        if tx.q.free_count() == 0 {
            while let Some((h, _)) = tx.q.pop_used() {
                tx.bufs[h as usize] = None;
            }
        }
        let addr = buf.as_ptr() as u64;
        match tx.q.submit(&[(addr, buf.len() as u32, false)]) {
            Some(h) => {
                tx.bufs[h as usize] = Some(buf);
                tx.q.notify();
                self.tx_frames.fetch_add(1, Ordering::Relaxed);
            }
            None => {
                self.tx_dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Polling (when there is no MSI-X, or during boot).
    pub fn poll(&self) {
        self.interrupt();
    }
}

pub fn probe_all() -> Option<&'static VirtioNet> {
    pci::devices().into_iter().filter(|p| virtio::device_type(p) == Some(1)).find_map(VirtioNet::probe)
}
