//! The TCP/IP stack: Ethernet, ARP, IPv4, ICMP echo, UDP and TCP over the virtio-net card,
//! plus a loopback path (127.0.0.0/8 and the card's own address).
//!
//! The protocol logic is in kcore::net (TCP is tested there over a lossy simulated link); this
//! module keeps the tables (ARP cache, connections, listeners, UDP ports), runs one kernel
//! thread that processes received frames and the TCP timers, and provides the socket calls.
//! One lock (STACK) covers the tables and is always taken before a connection's own lock.
pub mod socket;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kcore::errno::*;
use kcore::net::tcp::{self, Tcb};
use kcore::net::wire::{self, Mac, Segment};

use crate::drivers::net::VirtioNet;
use crate::sched::WaitQueue;
use crate::sync::{Once, SpinLock};

pub const LOOPBACK: u32 = 0x7f00_0001;

pub struct TcpSock {
    pub tcb: SpinLock<Tcb>,
    pub local_ip: u32,
    pub remote_ip: u32,
    pub wq: Arc<WaitQueue>,
    parent: Option<Weak<Listener>>,
    queued: AtomicBool,
    pub user_closed: AtomicBool,
}

pub struct Listener {
    pub port: u16,
    pub ip: u32,
    pub backlog: SpinLock<VecDeque<Arc<TcpSock>>>,
    pub max_backlog: usize,
    pub wq: Arc<WaitQueue>,
}

pub struct UdpSock {
    pub port: u16,
    pub queue: SpinLock<VecDeque<(u32, u16, Vec<u8>)>>,
    pub wq: Arc<WaitQueue>,
}

type ConnKey = (u16, u32, u16); // local port, remote ip, remote port

pub struct Stack {
    pub mac: Mac,
    pub ip: u32,
    pub mask: u32,
    pub gw: u32,
    arp: BTreeMap<u32, Mac>,
    arp_wait: Vec<(u32, Vec<u8>, u64)>,
    ip_id: u16,
    conns: BTreeMap<ConnKey, Arc<TcpSock>>,
    listeners: BTreeMap<u16, Arc<Listener>>,
    udp: BTreeMap<u16, Arc<UdpSock>>,
    next_port: u16,
}

static NIC: Once<Option<&'static VirtioNet>> = Once::new();
static STACK: Once<SpinLock<Stack>> = Once::new();
static RXQ: SpinLock<VecDeque<Vec<u8>>> = SpinLock::new(VecDeque::new());
static NET_WQ: WaitQueue = WaitQueue::new();
/// Counters: frames in, frames out, loopback packets, dropped, resets sent, ARP requests.
pub static STATS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

pub fn now() -> u64 {
    crate::arch::apic::now_ns()
}

pub fn stack() -> crate::sync::Guard<'static, Stack> {
    STACK.get().lock()
}

/// Frames from the card (interrupt context): queue them for the network thread.
pub fn receive(frames: Vec<Vec<u8>>) {
    RXQ.lock().extend(frames);
    NET_WQ.wake_all();
}

fn parse_ip(s: &str) -> Option<u32> {
    let mut v = [0u8; 4];
    let mut it = s.split('.');
    for b in v.iter_mut() {
        *b = it.next()?.parse().ok()?;
    }
    if it.next().is_some() {
        return None;
    }
    Some(u32::from_be_bytes(v))
}

fn fmt_ip(ip: u32) -> alloc::string::String {
    let b = ip.to_be_bytes();
    alloc::format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}

/// Brings up the card (if any) and the stack. Addresses default to QEMU's user network;
/// ip=10.0.2.15/24 gw=10.0.2.2 on the command line override them.
pub fn init() {
    let nic = crate::drivers::net::probe_all();
    NIC.set(nic);
    let bi = crate::BOOT_INFO.get();
    let (ip, prefix) = bi
        .arg("ip")
        .and_then(|s| {
            let (a, p) = s.split_once('/').unwrap_or((s, "24"));
            Some((parse_ip(a)?, p.parse::<u32>().ok()?))
        })
        .unwrap_or((wire::ip(10, 0, 2, 15), 24));
    let gw = bi.arg("gw").and_then(parse_ip).unwrap_or(wire::ip(10, 0, 2, 2));
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32)) };
    let mac = nic.map(|n| n.mac).unwrap_or([0x02, 0, 0, 0, 0, 1]);
    STACK.set(SpinLock::new(Stack {
        mac,
        ip: if nic.is_some() { ip } else { LOOPBACK },
        mask,
        gw,
        arp: BTreeMap::new(),
        arp_wait: Vec::new(),
        ip_id: 1,
        conns: BTreeMap::new(),
        listeners: BTreeMap::new(),
        udp: BTreeMap::new(),
        next_port: 49152,
    }));
    if nic.is_some() {
        crate::kprintln!("net: {}/{prefix} gateway {}", fmt_ip(ip), fmt_ip(gw));
    }
    crate::sched::spawn_kernel("net", net_thread);
}

fn net_thread() {
    let mut next_tick = now();
    loop {
        let frames: Vec<Vec<u8>> = RXQ.lock().drain(..).collect();
        if !frames.is_empty() {
            let mut st = stack();
            for f in frames {
                st.input(&f);
            }
        }
        let t = now();
        if t >= next_tick {
            stack().timers(t);
            next_tick = t + 10_000_000;
        }
        if RXQ.lock().is_empty() {
            crate::sched::arm_timer(next_tick);
            NET_WQ.wait_until(|| !RXQ.lock().is_empty() || now() >= next_tick);
        }
    }
}

fn is_local(st: &Stack, ip: u32) -> bool {
    ip >> 24 == 127 || ip == st.ip
}

fn iss() -> u32 {
    // A clock plus a mixed-in counter (RFC 6528 in spirit).
    static N: AtomicU64 = AtomicU64::new(0x9e37_79b9);
    let x = now() / 4000 + N.fetch_add(0x6a09_e667, Ordering::Relaxed);
    (x ^ (x >> 29)).wrapping_mul(0xbf58_476d_1ce4_e5b9) as u32
}

impl Stack {
    fn input(&mut self, frame: &[u8]) {
        STATS[0].fetch_add(1, Ordering::Relaxed);
        let Some(eth) = wire::parse_eth(frame) else { return };
        match eth.ethertype {
            wire::ETH_ARP => self.input_arp(eth.payload),
            wire::ETH_IPV4 => {
                if let Some(ip) = wire::parse_ipv4(eth.payload) {
                    if is_local(self, ip.dst) || ip.dst == u32::MAX || ip.dst == (self.ip | !self.mask) {
                        self.input_ip(&ip);
                    }
                }
            }
            _ => {}
        }
    }

    fn input_arp(&mut self, p: &[u8]) {
        let Some(a) = wire::parse_arp(p) else { return };
        self.arp.insert(a.spa, a.sha);
        if a.op == 1 && a.tpa == self.ip {
            let reply = wire::build_arp(&wire::Arp { op: 2, sha: self.mac, spa: self.ip, tha: a.sha, tpa: a.spa });
            self.send_frame(wire::build_eth(a.sha, self.mac, wire::ETH_ARP, &reply));
        }
        // Packets that were waiting for this address.
        let (go, stay): (Vec<_>, Vec<_>) = core::mem::take(&mut self.arp_wait).into_iter().partition(|w| w.0 == a.spa);
        self.arp_wait = stay;
        for w in go {
            self.send_frame(wire::build_eth(a.sha, self.mac, wire::ETH_IPV4, &w.1));
        }
    }

    fn input_ip(&mut self, ip: &wire::Ipv4) {
        match ip.proto {
            wire::PROTO_ICMP => {
                if let Some((8, id, seq, data)) = wire::parse_icmp_echo(ip.payload) {
                    let reply = wire::build_icmp_echo(0, id, seq, data);
                    self.send_ip(ip.dst, ip.src, wire::PROTO_ICMP, &reply);
                }
            }
            wire::PROTO_UDP => {
                let Some(u) = wire::parse_udp(ip.src, ip.dst, ip.payload) else { return };
                if let Some(s) = self.udp.get(&u.dport) {
                    let mut q = s.queue.lock();
                    if q.len() < 256 {
                        q.push_back((ip.src, u.sport, u.payload.to_vec()));
                    }
                    drop(q);
                    s.wq.wake_all();
                }
            }
            wire::PROTO_TCP => {
                let Some(seg) = wire::parse_tcp(ip.src, ip.dst, ip.payload) else {
                    STATS[3].fetch_add(1, Ordering::Relaxed);
                    return;
                };
                self.input_tcp(ip.dst, ip.src, seg);
            }
            _ => {}
        }
    }

    fn input_tcp(&mut self, dst: u32, src: u32, seg: Segment) {
        let t = now();
        let key = (seg.dport, src, seg.sport);
        if let Some(c) = self.conns.get(&key).cloned() {
            let out = {
                let mut tcb = c.tcb.lock();
                tcb.input(&seg, t);
                tcb.take_output()
            };
            self.send_segments(&c, out);
            self.after_change(&key, &c);
            return;
        }
        if seg.flags & wire::SYN != 0 && seg.flags & wire::ACK == 0 {
            if let Some(l) = self.listeners.get(&seg.dport).cloned() {
                if l.ip == 0 || l.ip == dst {
                    let wl = Arc::downgrade(&l);
                    let pending = self.conns.values().filter(|c| c.parent.as_ref().is_some_and(|p| p.ptr_eq(&wl)) && !c.queued.load(Ordering::Relaxed)).count();
                    if l.backlog.lock().len() + pending >= l.max_backlog {
                        STATS[3].fetch_add(1, Ordering::Relaxed);
                        return; // the client will retry
                    }
                    let tcb = Tcb::accept_syn(seg.dport, &seg, iss(), t);
                    let c = Arc::new(TcpSock {
                        tcb: SpinLock::new(tcb),
                        local_ip: dst,
                        remote_ip: src,
                        wq: Arc::new(WaitQueue::new()),
                        parent: Some(wl),
                        queued: AtomicBool::new(false),
                        user_closed: AtomicBool::new(false),
                    });
                    let out = c.tcb.lock().take_output();
                    self.conns.insert(key, c.clone());
                    self.send_segments(&c, out);
                    return;
                }
            }
        }
        // Nobody wants it: reset.
        if let Some(r) = tcp::reset_for(&seg) {
            STATS[4].fetch_add(1, Ordering::Relaxed);
            let bytes = wire::build_tcp(dst, src, &r);
            self.send_ip(dst, src, wire::PROTO_TCP, &bytes);
        }
    }

    /// Book-keeping after a connection's state may have changed: hand new connections to
    /// their listener, wake waiters, forget finished connections.
    fn after_change(&mut self, key: &ConnKey, c: &Arc<TcpSock>) {
        let (established, closed) = {
            let t = c.tcb.lock();
            (t.established(), t.is_closed())
        };
        if established && !c.queued.load(Ordering::Relaxed) {
            if let Some(l) = c.parent.as_ref().and_then(|p| p.upgrade()) {
                c.queued.store(true, Ordering::Relaxed);
                l.backlog.lock().push_back(c.clone());
                l.wq.wake_all();
            }
        }
        c.wq.wake_all();
        if closed && (c.user_closed.load(Ordering::Relaxed) || (c.parent.is_some() && !c.queued.load(Ordering::Relaxed))) {
            self.conns.remove(key);
        }
    }

    fn timers(&mut self, t: u64) {
        let list: Vec<(ConnKey, Arc<TcpSock>)> = self.conns.iter().map(|(k, v)| (*k, v.clone())).collect();
        for (k, c) in list {
            let out = {
                let mut tcb = c.tcb.lock();
                if tcb.next_timer().is_some_and(|at| at <= t) {
                    tcb.on_timer(t);
                }
                tcb.take_output()
            };
            if !out.is_empty() {
                self.send_segments(&c, out);
            }
            self.after_change(&k, &c);
        }
        // Give up on addresses that never answered ARP.
        self.arp_wait.retain(|w| t < w.2 + 3_000_000_000);
    }

    pub fn send_segments(&mut self, c: &TcpSock, segs: Vec<Segment>) {
        for s in segs {
            let bytes = wire::build_tcp(c.local_ip, c.remote_ip, &s);
            self.send_ip(c.local_ip, c.remote_ip, wire::PROTO_TCP, &bytes);
        }
    }

    pub fn send_ip(&mut self, src: u32, dst: u32, proto: u8, payload: &[u8]) {
        self.ip_id = self.ip_id.wrapping_add(1);
        let pkt = wire::build_ipv4(src, dst, proto, self.ip_id, payload);
        if is_local(self, dst) {
            // Loopback: back into our own input queue (processed by the network thread).
            STATS[2].fetch_add(1, Ordering::Relaxed);
            RXQ.lock().push_back(wire::build_eth(self.mac, self.mac, wire::ETH_IPV4, &pkt));
            NET_WQ.wake_all();
            return;
        }
        let hop = if dst & self.mask == self.ip & self.mask || dst == u32::MAX { dst } else { self.gw };
        let mac = if dst == u32::MAX { Some(wire::BROADCAST) } else { self.arp.get(&hop).copied() };
        match mac {
            Some(m) => self.send_frame(wire::build_eth(m, self.mac, wire::ETH_IPV4, &pkt)),
            None => {
                if self.arp_wait.len() < 64 {
                    self.arp_wait.push((hop, pkt, now()));
                }
                STATS[5].fetch_add(1, Ordering::Relaxed);
                let req = wire::build_arp(&wire::Arp { op: 1, sha: self.mac, spa: self.ip, tha: [0; 6], tpa: hop });
                self.send_frame(wire::build_eth(wire::BROADCAST, self.mac, wire::ETH_ARP, &req));
            }
        }
    }

    fn send_frame(&mut self, f: Vec<u8>) {
        STATS[1].fetch_add(1, Ordering::Relaxed);
        if let Some(n) = NIC.get() {
            n.send(&f);
        }
    }

    pub fn ephemeral_port(&mut self) -> u16 {
        for _ in 0..16384 {
            let p = self.next_port;
            self.next_port = if self.next_port == 65535 { 49152 } else { self.next_port + 1 };
            if !self.listeners.contains_key(&p) && !self.udp.contains_key(&p) && !self.conns.keys().any(|k| k.0 == p) {
                return p;
            }
        }
        0
    }

    pub fn port_in_use(&self, port: u16, stream: bool) -> bool {
        if stream { self.listeners.contains_key(&port) } else { self.udp.contains_key(&port) }
    }

    /// Opens a connection (sends SYN).
    pub fn connect(&mut self, local_ip: u32, local_port: u16, ip: u32, port: u16, wq: Arc<WaitQueue>) -> KResult<Arc<TcpSock>> {
        let key = (local_port, ip, port);
        if self.conns.contains_key(&key) {
            return Err(EADDRINUSE);
        }
        let c = Arc::new(TcpSock {
            tcb: SpinLock::new(Tcb::connect(local_port, port, iss(), now())),
            local_ip,
            remote_ip: ip,
            wq,
            parent: None,
            queued: AtomicBool::new(true),
            user_closed: AtomicBool::new(false),
        });
        let out = c.tcb.lock().take_output();
        self.conns.insert(key, c.clone());
        self.send_segments(&c, out);
        Ok(c)
    }

    pub fn listen(&mut self, ip: u32, port: u16, backlog: usize, wq: Arc<WaitQueue>) -> KResult<Arc<Listener>> {
        if self.listeners.contains_key(&port) {
            return Err(EADDRINUSE);
        }
        let l = Arc::new(Listener { port, ip, backlog: SpinLock::new(VecDeque::new()), max_backlog: backlog.clamp(1, 1024), wq });
        self.listeners.insert(port, l.clone());
        Ok(l)
    }

    pub fn unlisten(&mut self, l: &Arc<Listener>) {
        if self.listeners.get(&l.port).is_some_and(|x| Arc::ptr_eq(x, l)) {
            self.listeners.remove(&l.port);
        }
        // Connections nobody accepted are reset.
        let queued: Vec<Arc<TcpSock>> = l.backlog.lock().drain(..).collect();
        for c in queued {
            self.close_conn(&c, true);
        }
    }

    pub fn bind_udp(&mut self, port: u16, wq: Arc<WaitQueue>) -> KResult<Arc<UdpSock>> {
        if self.udp.contains_key(&port) {
            return Err(EADDRINUSE);
        }
        let u = Arc::new(UdpSock { port, queue: SpinLock::new(VecDeque::new()), wq });
        self.udp.insert(port, u.clone());
        Ok(u)
    }

    pub fn unbind_udp(&mut self, u: &Arc<UdpSock>) {
        if self.udp.get(&u.port).is_some_and(|x| Arc::ptr_eq(x, u)) {
            self.udp.remove(&u.port);
        }
    }

    /// The user closed the socket: orderly FIN, or RST if unread data would be lost.
    pub fn close_conn(&mut self, c: &Arc<TcpSock>, abort: bool) {
        c.user_closed.store(true, Ordering::Relaxed);
        let (out, key) = {
            let mut t = c.tcb.lock();
            if abort || t.rx_len() > 0 {
                t.abort();
            } else {
                t.close(now());
            }
            (t.take_output(), (t.local_port, c.remote_ip, t.remote_port))
        };
        self.send_segments(c, out);
        self.after_change(&key, c);
    }

    /// Runs `f` on a connection and transmits what it produced.
    pub fn with_conn<R>(&mut self, c: &Arc<TcpSock>, f: impl FnOnce(&mut Tcb) -> R) -> R {
        let (r, out) = {
            let mut t = c.tcb.lock();
            let r = f(&mut t);
            (r, t.take_output())
        };
        if !out.is_empty() {
            self.send_segments(c, out);
        }
        r
    }

    pub fn conn_count(&self) -> usize {
        self.conns.len()
    }
}

pub use socket::syscall;
