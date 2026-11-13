//! Sockets (AF_INET, SOCK_STREAM and SOCK_DGRAM) as file-system inodes, and the socket calls.
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use kcore::errno::*;

use super::{now, stack, Listener, TcpSock, UdpSock, LOOPBACK};
use crate::fs::{File, Inode, Kind, Stat, POLLERR, POLLHUP, POLLIN, POLLOUT};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::user;

const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
const SOCK_NONBLOCK: u64 = 0o4000;
const SOCK_CLOEXEC: u64 = 0o2000000;
const SOL_SOCKET: u64 = 1;
const SO_TYPE: u64 = 3;
const SO_ERROR: u64 = 4;

enum State {
    /// Created, maybe bound: (ip, port).
    Fresh { stream: bool, bound: Option<(u32, u16)> },
    Listening(Arc<Listener>),
    Conn(Arc<TcpSock>),
    Udp { sock: Arc<UdpSock>, ip: u32, peer: Option<(u32, u16)> },
    Closed,
}

pub struct Socket {
    state: SpinLock<State>,
    nonblock: AtomicBool,
    wq: Arc<WaitQueue>,
    ino: u64,
    stream: bool,
}

impl Socket {
    fn new(stream: bool) -> Arc<Socket> {
        Arc::new(Socket { state: SpinLock::new(State::Fresh { stream, bound: None }), nonblock: AtomicBool::new(false), wq: Arc::new(WaitQueue::new()), ino: crate::fs::new_ino(), stream })
    }

    fn from_conn(c: Arc<TcpSock>) -> Arc<Socket> {
        let wq = c.wq.clone();
        Arc::new(Socket { state: SpinLock::new(State::Conn(c)), nonblock: AtomicBool::new(false), wq, ino: crate::fs::new_ino(), stream: true })
    }

    pub fn set_nonblock(&self, on: bool) {
        self.nonblock.store(on, Ordering::Relaxed);
    }

    fn conn(&self) -> Option<Arc<TcpSock>> {
        match &*self.state.lock() {
            State::Conn(c) => Some(c.clone()),
            _ => None,
        }
    }

    fn udp(&self) -> Option<(Arc<UdpSock>, u32, Option<(u32, u16)>)> {
        match &*self.state.lock() {
            State::Udp { sock, ip, peer } => Some((sock.clone(), *ip, *peer)),
            _ => None,
        }
    }

    /// Binds a datagram socket on first use.
    fn ensure_udp(&self) -> KResult<(Arc<UdpSock>, u32, Option<(u32, u16)>)> {
        if let Some(u) = self.udp() {
            return Ok(u);
        }
        let mut st = stack();
        let mut s = self.state.lock();
        let bound = match &*s {
            State::Fresh { stream: false, bound } => *bound,
            _ => return Err(EINVAL),
        };
        let (ip, port) = bound.unwrap_or((0, 0));
        let port = if port == 0 { st.ephemeral_port() } else { port };
        let sock = st.bind_udp(port, self.wq.clone())?;
        *s = State::Udp { sock: sock.clone(), ip, peer: None };
        Ok((sock, ip, None))
    }

    fn recv_stream(&self, buf: &mut [u8]) -> KResult<usize> {
        let c = self.conn().ok_or(ENOTCONN)?;
        loop {
            let (r, eof) = {
                let mut st = stack();
                let t = now();
                st.with_conn(&c, |tcb| {
                    let r = tcb.recv(buf, t);
                    let eof = tcb.readable() && tcb.rx_len() == 0;
                    (r, eof)
                })
            };
            match r {
                Ok(0) if eof || buf.is_empty() => return Ok(0),
                Ok(0) => {}
                other => return other,
            }
            if self.nonblock.load(Ordering::Relaxed) {
                return Err(EAGAIN);
            }
            if !c.wq.wait_until(|| c.tcb.lock().readable()) {
                return Err(EINTR);
            }
        }
    }

    fn send_stream(&self, data: &[u8]) -> KResult<usize> {
        let c = self.conn().ok_or(ENOTCONN)?;
        let mut done = 0;
        while done < data.len() {
            let r = {
                let mut st = stack();
                let t = now();
                st.with_conn(&c, |tcb| tcb.send(&data[done..], t))
            };
            match r {
                Ok(0) => {}
                Ok(n) => {
                    done += n;
                    continue;
                }
                Err(e) => {
                    if done > 0 {
                        return Ok(done);
                    }
                    if e == EPIPE {
                        crate::signal::send_current(crate::signal::SIGPIPE);
                    }
                    return Err(e);
                }
            }
            if self.nonblock.load(Ordering::Relaxed) {
                return if done > 0 { Ok(done) } else { Err(EAGAIN) };
            }
            if !c.wq.wait_until(|| c.tcb.lock().writable()) {
                return if done > 0 { Ok(done) } else { Err(EINTR) };
            }
        }
        Ok(done)
    }

    fn sendto(&self, data: &[u8], to: Option<(u32, u16)>) -> KResult<usize> {
        if self.stream {
            return self.send_stream(data);
        }
        let (sock, ip, peer) = self.ensure_udp()?;
        let (dip, dport) = to.or(peer).ok_or(EDESTADDRREQ)?;
        if data.len() > 1472 {
            return Err(EMSGSIZE);
        }
        let mut st = stack();
        let src = if ip != 0 { ip } else if dip >> 24 == 127 { LOOPBACK } else { st.ip };
        let pkt = kcore::net::wire::build_udp(src, dip, sock.port, dport, data);
        st.send_ip(src, dip, kcore::net::wire::PROTO_UDP, &pkt);
        Ok(data.len())
    }

    fn recvfrom(&self, buf: &mut [u8]) -> KResult<(usize, Option<(u32, u16)>)> {
        if self.stream {
            return self.recv_stream(buf).map(|n| (n, None));
        }
        let (sock, _, _) = self.ensure_udp()?;
        loop {
            if let Some((ip, port, data)) = sock.queue.lock().pop_front() {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                return Ok((n, Some((ip, port))));
            }
            if self.nonblock.load(Ordering::Relaxed) {
                return Err(EAGAIN);
            }
            if !sock.wq.wait_until(|| !sock.queue.lock().is_empty()) {
                return Err(EINTR);
            }
        }
    }

    fn local_addr(&self) -> (u32, u16) {
        match &*self.state.lock() {
            State::Fresh { bound, .. } => bound.unwrap_or((0, 0)),
            State::Listening(l) => (l.ip, l.port),
            State::Conn(c) => (c.local_ip, c.tcb.lock().local_port),
            State::Udp { sock, ip, .. } => (*ip, sock.port),
            State::Closed => (0, 0),
        }
    }

    fn peer_addr(&self) -> KResult<(u32, u16)> {
        match &*self.state.lock() {
            State::Conn(c) => Ok((c.remote_ip, c.tcb.lock().remote_port)),
            State::Udp { peer: Some(p), .. } => Ok(*p),
            _ => Err(ENOTCONN),
        }
    }
}

impl Inode for Socket {
    fn kind(&self) -> Kind {
        Kind::Socket
    }
    fn stat(&self) -> Stat {
        Stat { dev: 0x50, ino: self.ino, mode: 0o777, nlink: 1, ..Default::default() }
    }
    fn read_at(&self, _off: u64, buf: &mut [u8]) -> KResult<usize> {
        self.recvfrom(buf).map(|r| r.0)
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> KResult<usize> {
        self.sendto(buf, None)
    }
    fn poll(&self) -> u16 {
        match &*self.state.lock() {
            State::Listening(l) => {
                if l.backlog.lock().is_empty() { 0 } else { POLLIN }
            }
            State::Conn(c) => {
                let t = c.tcb.lock();
                let mut r = 0;
                if t.readable() {
                    r |= POLLIN;
                }
                if t.writable() {
                    r |= POLLOUT;
                }
                if t.error.is_some() {
                    r |= POLLERR;
                }
                if t.is_closed() {
                    r |= POLLHUP;
                }
                r
            }
            State::Udp { sock, .. } => POLLOUT | if sock.queue.lock().is_empty() { 0 } else { POLLIN },
            State::Fresh { .. } => POLLOUT,
            State::Closed => POLLHUP,
        }
    }
    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&self.wq)
    }
    fn release(&self) {
        let old = core::mem::replace(&mut *self.state.lock(), State::Closed);
        let mut st = stack();
        match old {
            State::Listening(l) => st.unlisten(&l),
            State::Conn(c) => st.close_conn(&c, false),
            State::Udp { sock, .. } => st.unbind_udp(&sock),
            _ => {}
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------- system calls

fn read_addr(addr: u64, len: u64) -> KResult<(u32, u16)> {
    if len < 16 {
        return Err(EINVAL);
    }
    let mut b = [0u8; 16];
    user::copy_in(addr, &mut b)?;
    if u16::from_le_bytes([b[0], b[1]]) as u64 != AF_INET {
        return Err(EAFNOSUPPORT);
    }
    Ok((u32::from_be_bytes([b[4], b[5], b[6], b[7]]), u16::from_be_bytes([b[2], b[3]])))
}

fn write_addr(addr: u64, lenp: u64, a: (u32, u16)) -> KResult<()> {
    if addr == 0 {
        return Ok(());
    }
    let mut b = [0u8; 16];
    b[0..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
    b[2..4].copy_from_slice(&a.1.to_be_bytes());
    b[4..8].copy_from_slice(&a.0.to_be_bytes());
    let cap = user::get_u32(lenp)? as usize;
    user::copy_out(addr, &b[..cap.min(16)])?;
    user::put_u32(lenp, 16)
}

fn sock(fd: u64) -> KResult<(Arc<File>, Arc<Socket>)> {
    let f = crate::proc::current().fds.lock().get(fd as i64)?;
    let inode = f.inode.clone();
    if inode.as_any().downcast_ref::<Socket>().is_none() {
        return Err(ENOTSOCK);
    }
    // Recover the Arc<Socket> from the Arc<dyn Inode>.
    let raw = Arc::into_raw(inode) as *const Socket;
    Ok((f, unsafe { Arc::from_raw(raw) }))
}

fn new_fd(s: Arc<Socket>, flags: u64) -> KResult<u64> {
    let nb = flags & SOCK_NONBLOCK != 0;
    s.set_nonblock(nb);
    let fl = crate::fs::O_RDWR | if nb { crate::fs::O_NONBLOCK } else { 0 };
    let f = File::new(s.clone(), fl, alloc::format!("socket:[{}]", s.ino));
    crate::proc::current().fds.lock().alloc(f, flags & SOCK_CLOEXEC != 0, 0).map(|fd| fd as u64)
}

pub fn syscall(nr: u64, a: &[u64; 6]) -> KResult<u64> {
    match nr {
        41 => {
            if a[0] != AF_INET {
                return Err(EAFNOSUPPORT);
            }
            let stream = match a[1] & 0xf {
                SOCK_STREAM => true,
                SOCK_DGRAM => false,
                _ => return Err(EINVAL),
            };
            new_fd(Socket::new(stream), a[1])
        }
        49 => {
            let (_, s) = sock(a[0])?;
            let (ip, port) = read_addr(a[1], a[2])?;
            let st = stack();
            if port != 0 && st.port_in_use(port, s.stream) {
                return Err(EADDRINUSE);
            }
            if ip != 0 && ip >> 24 != 127 && ip != st.ip {
                return Err(EADDRNOTAVAIL);
            }
            drop(st);
            match &mut *s.state.lock() {
                State::Fresh { bound, .. } if bound.is_none() => *bound = Some((ip, port)),
                _ => return Err(EINVAL),
            }
            Ok(0)
        }
        50 => {
            let (_, s) = sock(a[0])?;
            let mut st = stack();
            let mut state = s.state.lock();
            let (ip, port) = match &*state {
                State::Fresh { stream: true, bound } => bound.unwrap_or((0, 0)),
                State::Listening(_) => return Ok(0),
                _ => return Err(EINVAL),
            };
            let port = if port == 0 { st.ephemeral_port() } else { port };
            let l = st.listen(ip, port, (a[1] as usize).max(16), s.wq.clone())?;
            *state = State::Listening(l);
            Ok(0)
        }
        43 | 288 => {
            let (_, s) = sock(a[0])?;
            let l = match &*s.state.lock() {
                State::Listening(l) => l.clone(),
                _ => return Err(EINVAL),
            };
            let flags = if nr == 288 { a[3] } else { 0 };
            loop {
                if let Some(c) = l.backlog.lock().pop_front() {
                    let peer = (c.remote_ip, c.tcb.lock().remote_port);
                    let ns = Socket::from_conn(c);
                    let fd = new_fd(ns, flags)?;
                    if a[1] != 0 {
                        write_addr(a[1], a[2], peer)?;
                    }
                    return Ok(fd);
                }
                if s.nonblock.load(Ordering::Relaxed) {
                    return Err(EAGAIN);
                }
                if !l.wq.wait_until(|| !l.backlog.lock().is_empty()) {
                    return Err(EINTR);
                }
            }
        }
        42 => {
            let (_, s) = sock(a[0])?;
            let (ip, port) = read_addr(a[1], a[2])?;
            if !s.stream {
                let _ = s.ensure_udp()?;
                if let State::Udp { peer, .. } = &mut *s.state.lock() {
                    *peer = Some((ip, port));
                }
                return Ok(0);
            }
            let c = {
                let mut st = stack();
                let mut state = s.state.lock();
                let (lip, lport) = match &*state {
                    State::Fresh { bound, .. } => bound.unwrap_or((0, 0)),
                    State::Conn(_) => return Err(EISCONN),
                    _ => return Err(EINVAL),
                };
                let lip = if lip != 0 { lip } else if ip >> 24 == 127 { LOOPBACK } else { st.ip };
                let lport = if lport == 0 { st.ephemeral_port() } else { lport };
                let c = st.connect(lip, lport, ip, port, s.wq.clone())?;
                *state = State::Conn(c.clone());
                c
            };
            if s.nonblock.load(Ordering::Relaxed) {
                return Err(EINPROGRESS);
            }
            if !c.wq.wait_until(|| {
                let t = c.tcb.lock();
                t.established() || t.error.is_some() || t.is_closed()
            }) {
                return Err(EINTR);
            }
            let t = c.tcb.lock();
            match t.error {
                Some(e) => Err(e),
                None if t.established() => Ok(0),
                None => Err(ECONNREFUSED),
            }
        }
        44 => {
            let (_, s) = sock(a[0])?;
            let data = unsafe { user::slice(a[1], a[2] as usize)? };
            let to = if a[4] != 0 && !s.stream { Some(read_addr(a[4], a[5])?) } else { None };
            s.sendto(data, to).map(|n| n as u64)
        }
        45 => {
            let (_, s) = sock(a[0])?;
            let buf = unsafe { user::slice_mut(a[1], a[2] as usize)? };
            let (n, from) = s.recvfrom(buf)?;
            if let Some(f) = from {
                write_addr(a[4], a[5], f)?;
            }
            Ok(n as u64)
        }
        48 => {
            let (_, s) = sock(a[0])?;
            let c = s.conn().ok_or(ENOTCONN)?;
            if a[1] >= 1 {
                let t = now();
                stack().with_conn(&c, |tcb| tcb.close(t));
            }
            Ok(0)
        }
        51 => {
            let (_, s) = sock(a[0])?;
            let mut addr = s.local_addr();
            if addr.0 == 0 {
                addr.0 = stack().ip;
            }
            write_addr(a[1], a[2], addr)?;
            Ok(0)
        }
        52 => {
            let (_, s) = sock(a[0])?;
            write_addr(a[1], a[2], s.peer_addr()?)?;
            Ok(0)
        }
        54 => {
            sock(a[0])?;
            Ok(0) // SO_REUSEADDR, TCP_NODELAY, SO_KEEPALIVE...: accepted
        }
        55 => {
            let (_, s) = sock(a[0])?;
            let v: u32 = match (a[1], a[2]) {
                (SOL_SOCKET, SO_ERROR) => s.conn().and_then(|c| c.tcb.lock().error).unwrap_or(0) as u32,
                (SOL_SOCKET, SO_TYPE) => if s.stream { 1 } else { 2 },
                _ => 0,
            };
            user::put_u32(a[3], v)?;
            user::put_u32(a[4], 4)?;
            Ok(0)
        }
        _ => {
            let _ = String::new();
            Err(EOPNOTSUPP)
        }
    }
}
