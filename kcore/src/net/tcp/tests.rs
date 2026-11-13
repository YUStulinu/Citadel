//! TCP over a simulated network that loses, duplicates, delays and reorders segments, and
//! round-trips every segment through the wire format (checksums included).
use super::*;
use crate::net::wire;

const MS_: u64 = 1_000_000;
const CIP: u32 = 0x0a00_0001;
const SIP: u32 = 0x0a00_0002;

struct Link {
    rng: u64,
    loss: u64,    // per mille
    dup: u64,     // per mille
    jitter: u64,  // max extra delay in ns
    queue: Vec<(u64, bool, Vec<u8>)>, // (deliver at, to server, IPv4 TCP bytes)
    sent: u64,
}

impl Link {
    fn rand(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }
    fn send(&mut self, now: u64, to_server: bool, seg: &Segment) {
        self.sent += 1;
        let (src, dst) = if to_server { (CIP, SIP) } else { (SIP, CIP) };
        let bytes = wire::build_tcp(src, dst, seg);
        if self.rand() % 1000 < self.loss {
            return;
        }
        let copies = if self.rand() % 1000 < self.dup { 2 } else { 1 };
        for _ in 0..copies {
            let delay = MS_ + if self.jitter > 0 { self.rand() % self.jitter } else { 0 };
            self.queue.push((now + delay, to_server, bytes.clone()));
        }
    }
    fn due(&mut self, now: u64) -> Vec<(bool, Segment)> {
        // Stable: segments due at the same moment keep their sending order (only jitter reorders).
        let (mut due, keep): (Vec<_>, Vec<_>) = core::mem::take(&mut self.queue).into_iter().partition(|q| q.0 <= now);
        self.queue = keep;
        due.sort_by_key(|q| q.0);
        due.into_iter()
            .map(|(_, to_server, bytes)| {
                let (src, dst) = if to_server { (CIP, SIP) } else { (SIP, CIP) };
                (to_server, wire::parse_tcp(src, dst, &bytes).expect("checksum"))
            })
            .collect()
    }
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u32).wrapping_mul(2654435761).wrapping_add(seed as u32).to_le_bytes()[1]).collect()
}

struct Outcome {
    client_got: Vec<u8>,
    server_got: Vec<u8>,
    client: Tcb,
    server: Tcb,
    elapsed: u64,
}

/// Client sends `up`, server sends `down`; both close when done. Runs until both are Closed.
fn transfer(loss: u64, dup: u64, jitter: u64, up: &[u8], down: &[u8], server_reads_late: bool) -> Outcome {
    let mut link = Link { rng: 0x1234_5678_9abc_def1 ^ (loss << 8) ^ dup, loss, dup, jitter, queue: Vec::new(), sent: 0 };
    let mut now = 0u64;
    // Initial sequence numbers near the wrap-around point exercise modular comparisons.
    let mut client = Tcb::connect(40000, 80, u32::MAX - 1000, now);
    let mut server: Option<Tcb> = None;
    let (mut up_off, mut down_off) = (0usize, 0usize);
    let (mut client_got, mut server_got) = (Vec::new(), Vec::new());
    let (mut client_closed, mut server_closed) = (false, false);
    let mut buf = vec![0u8; 7000];
    for _ in 0..2_000_000 {
        for s in client.take_output() {
            link.send(now, true, &s);
        }
        if let Some(sv) = server.as_mut() {
            for s in sv.take_output() {
                link.send(now, false, &s);
            }
        }
        for (to_server, seg) in link.due(now) {
            if to_server {
                match server.as_mut() {
                    Some(sv) => sv.input(&seg, now),
                    None if seg.flags & SYN != 0 => server = Some(Tcb::accept_syn(80, &seg, 0x7fff_fff0, now)),
                    None => {}
                }
            } else {
                client.input(&seg, now);
            }
        }
        // Applications.
        if client.established() && up_off < up.len() && client.writable() {
            up_off += client.send(&up[up_off..(up_off + 5000).min(up.len())], now).unwrap();
        }
        if client.established() && up_off == up.len() && !client_closed && client_got.len() == down.len() {
            client.close(now);
            client_closed = true;
        }
        if let Ok(n) = client.recv(&mut buf, now) {
            client_got.extend_from_slice(&buf[..n]);
        }
        if let Some(sv) = server.as_mut() {
            if sv.established() && down_off < down.len() && sv.writable() {
                down_off += sv.send(&down[down_off..(down_off + 3000).min(down.len())], now).unwrap();
            }
            let reading = !server_reads_late || now > 3000 * MS_;
            if reading {
                if let Ok(n) = sv.recv(&mut buf, now) {
                    server_got.extend_from_slice(&buf[..n]);
                }
            }
            if sv.state == State::CloseWait && down_off == down.len() && !server_closed {
                sv.close(now);
                server_closed = true;
            }
        }
        client.on_timer(now);
        if let Some(sv) = server.as_mut() {
            sv.on_timer(now);
        }
        if client.is_closed() && server.as_ref().is_some_and(|s| s.is_closed()) {
            break;
        }
        now += 100_000; // 0.1 ms steps
    }
    Outcome { client_got, server_got, client, server: server.expect("no connection"), elapsed: now }
}

#[test]
fn perfect_link_transfers_both_ways_and_closes() {
    let up = pattern(300_000, 1);
    let down = pattern(200_000, 2);
    let o = transfer(0, 0, 0, &up, &down, false);
    assert_eq!(o.server_got, up);
    assert_eq!(o.client_got, down);
    assert!(o.client.is_closed() && o.server.is_closed());
    assert_eq!(o.client.error, None);
    assert_eq!(o.server.error, None);
    assert_eq!(o.client.retransmits + o.server.retransmits, 0, "timeouts c={} s={} states {:?} {:?} t={}", o.client.timeouts, o.server.timeouts, o.client.state, o.server.state, o.elapsed);
}

#[test]
fn survives_loss_duplication_and_reordering() {
    for (loss, dup, jitter) in [(20, 0, 0), (50, 20, 3 * MS_), (100, 50, 10 * MS_)] {
        let up = pattern(150_000, 3);
        let down = pattern(90_000, 4);
        let o = transfer(loss, dup, jitter, &up, &down, false);
        assert_eq!(o.server_got.len(), up.len(), "loss {loss}");
        assert!(o.server_got == up, "corrupted upload at loss {loss}");
        assert!(o.client_got == down, "corrupted download at loss {loss}");
        assert!(o.client.is_closed() && o.server.is_closed(), "loss {loss}: {:?} {:?}", o.client.state, o.server.state);
        assert!(o.client.retransmits + o.server.retransmits > 0);
    }
}

#[test]
fn zero_window_is_probed_until_the_reader_catches_up() {
    // The server does not read for 3 s: the client fills the 64 KiB window and must wait.
    let up = pattern(200_000, 5);
    let o = transfer(0, 0, 0, &up, &[], true);
    assert!(o.server_got == up);
    assert!(o.elapsed > 3000 * MS_);
    assert!(o.client.is_closed() && o.server.is_closed());
}

#[test]
fn closed_port_refuses() {
    let mut c = Tcb::connect(5000, 9, 100, 0);
    let syn = c.take_output().remove(0);
    let rst = reset_for(&syn).unwrap();
    assert_eq!(rst.flags, RST | ACK);
    let rst = Segment { sport: 9, dport: 5000, ..rst };
    c.input(&rst, 1);
    assert_eq!(c.error, Some(ECONNREFUSED));
    assert!(c.is_closed());
    assert_eq!(c.send(b"x", 2), Err(ECONNREFUSED));
}

#[test]
fn unanswered_syn_times_out() {
    let mut c = Tcb::connect(5000, 9, 100, 0);
    let mut now = 0;
    let mut syns = 0;
    while !c.is_closed() && now < 3_600_000 * MS_ {
        syns += c.take_output().iter().filter(|s| s.flags & SYN != 0).count();
        now += 100 * MS_;
        c.on_timer(now);
    }
    assert_eq!(c.error, Some(ETIMEDOUT));
    assert!(syns > 5, "{syns} SYNs");
}

#[test]
fn reset_aborts_an_established_connection() {
    let o = transfer(0, 0, 0, b"ping", b"pong", false);
    assert!(o.client.is_closed());
    let mut c = Tcb::connect(1, 2, 10, 0);
    let syn = c.take_output().remove(0);
    let mut s = Tcb::accept_syn(2, &syn, 500, 0);
    for seg in s.take_output() {
        c.input(&seg, 1);
    }
    for seg in c.take_output() {
        s.input(&seg, 2);
    }
    assert_eq!(s.state, State::Established);
    s.abort();
    for seg in s.take_output() {
        c.input(&seg, 3);
    }
    assert_eq!(c.error, Some(ECONNRESET));
    assert!(c.readable());
    let mut b = [0u8; 4];
    assert_eq!(c.recv(&mut b, 4), Err(ECONNRESET));
}
