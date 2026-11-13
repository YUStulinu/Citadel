//! TCP (RFC 793/9293 core) for one connection, without I/O: segments come in through
//! [`Tcb::input`], the user side through [`Tcb::send`] / [`Tcb::recv`] / [`Tcb::close`], time
//! through [`Tcb::on_timer`], and whatever must be transmitted collects in [`Tcb::take_output`].
//!
//! Implemented: three-way handshake (active and passive), MSS option, sliding windows (no window
//! scaling: windows up to 64 KiB), retransmission with RFC 6298 RTO estimation and exponential
//! backoff, fast retransmit after three duplicate ACKs, out-of-order reassembly, zero-window
//! probes, orderly close in every direction (including simultaneous close), TIME_WAIT, RST.
//! Not implemented: urgent data, SACK, timestamps, window scaling, congestion control beyond
//! the receiver's window (the stack talks to QEMU's user network and to itself).
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use super::wire::{Segment, ACK, FIN, PSH, RST, SYN};
use crate::errno::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Closed,
    SynSent,
    SynRcvd,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

pub const OUR_MSS: u16 = 1460;
const DEFAULT_MSS: u16 = 536;
const MS: u64 = 1_000_000;
const RTO_INITIAL: u64 = 1000 * MS;
const RTO_MIN: u64 = 200 * MS;
const RTO_MAX: u64 = 60_000 * MS;
const TIME_WAIT: u64 = 2000 * MS;
/// Delayed ACK (RFC 1122 4.2.3.2): an ACK for in-order data waits this long for data to ride on,
/// unless a second full segment arrives first.
const DELACK: u64 = 20 * MS;
const MAX_RETRIES: u32 = 12;
pub const BUFFER: usize = 64 * 1024;

fn lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}
fn le(a: u32, b: u32) -> bool {
    !lt(b, a)
}

pub struct Tcb {
    pub state: State,
    pub local_port: u16,
    pub remote_port: u16,
    iss: u32,
    snd_una: u32,
    snd_nxt: u32,
    snd_wnd: u32,
    snd_wl1: u32,
    snd_wl2: u32,
    rcv_nxt: u32,
    /// Unacknowledged and unsent data; tx[0] has sequence number `tx_seq`.
    tx: VecDeque<u8>,
    tx_seq: u32,
    rx: VecDeque<u8>,
    ooo: BTreeMap<u32, Vec<u8>>,
    peer_mss: u16,
    fin_pending: bool,
    fin_sent: bool,
    fin_seq: u32,
    fin_acked: bool,
    peer_fin: bool,
    rto: u64,
    srtt: Option<u64>,
    rttvar: u64,
    rtt_probe: Option<(u32, u64)>,
    rtx_at: Option<u64>,
    retries: u32,
    dup_acks: u32,
    ack_now: bool,
    ack_at: Option<u64>,
    unacked_segs: u32,
    adv_wnd: u32,
    time_wait_end: u64,
    /// Why the connection ended abnormally (ECONNREFUSED, ECONNRESET, ETIMEDOUT).
    pub error: Option<i64>,
    out: Vec<Segment>,
    pub retransmits: u64,
    pub timeouts: u64,
}

impl Tcb {
    fn new(state: State, local_port: u16, remote_port: u16, iss: u32) -> Tcb {
        Tcb {
            state,
            local_port,
            remote_port,
            iss,
            snd_una: iss,
            snd_nxt: iss.wrapping_add(1),
            snd_wnd: DEFAULT_MSS as u32,
            snd_wl1: 0,
            snd_wl2: 0,
            rcv_nxt: 0,
            tx: VecDeque::new(),
            tx_seq: iss.wrapping_add(1),
            rx: VecDeque::new(),
            ooo: BTreeMap::new(),
            peer_mss: DEFAULT_MSS,
            fin_pending: false,
            fin_sent: false,
            fin_seq: 0,
            fin_acked: false,
            peer_fin: false,
            rto: RTO_INITIAL,
            srtt: None,
            rttvar: 0,
            rtt_probe: None,
            rtx_at: None,
            retries: 0,
            dup_acks: 0,
            ack_now: false,
            ack_at: None,
            unacked_segs: 0,
            adv_wnd: BUFFER as u32,
            time_wait_end: 0,
            error: None,
            out: Vec::new(),
            retransmits: 0,
            timeouts: 0,
        }
    }

    /// Active open: sends SYN.
    pub fn connect(local_port: u16, remote_port: u16, iss: u32, now: u64) -> Tcb {
        let mut t = Tcb::new(State::SynSent, local_port, remote_port, iss);
        t.send_syn(now);
        t
    }

    /// Passive open from a listener that received `syn`: replies SYN-ACK.
    pub fn accept_syn(local_port: u16, syn: &Segment, iss: u32, now: u64) -> Tcb {
        let mut t = Tcb::new(State::SynRcvd, local_port, syn.sport, iss);
        t.rcv_nxt = syn.seq.wrapping_add(1);
        t.peer_mss = syn.mss.unwrap_or(DEFAULT_MSS).clamp(64, OUR_MSS);
        t.snd_wnd = syn.window as u32;
        t.snd_wl1 = syn.seq;
        t.send_syn(now);
        t
    }

    fn window(&self) -> u16 {
        (BUFFER - self.rx.len()).min(65535) as u16
    }

    fn emit(&mut self, seq: u32, flags: u8, payload: Vec<u8>, mss: Option<u16>) {
        let ack = if flags & ACK != 0 { self.rcv_nxt } else { 0 };
        let window = self.window();
        self.adv_wnd = window as u32;
        self.out.push(Segment { sport: self.local_port, dport: self.remote_port, seq, ack, flags, window, mss, payload });
        if flags & ACK != 0 {
            self.ack_now = false;
            self.ack_at = None;
            self.unacked_segs = 0;
        }
    }

    fn send_syn(&mut self, now: u64) {
        let flags = if self.state == State::SynRcvd { SYN | ACK } else { SYN };
        self.emit(self.iss, flags, Vec::new(), Some(OUR_MSS));
        if self.rtx_at.is_none() {
            self.rtx_at = Some(now + self.rto);
        }
    }

    /// Segments to transmit (in order).
    pub fn take_output(&mut self) -> Vec<Segment> {
        core::mem::take(&mut self.out)
    }

    /// When `on_timer` next needs to run.
    pub fn next_timer(&self) -> Option<u64> {
        match self.state {
            State::TimeWait => Some(self.time_wait_end),
            _ => match (self.rtx_at, self.ack_at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
        }
    }

    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }

    /// Data (or end of stream, or an error) is waiting for recv().
    pub fn readable(&self) -> bool {
        !self.rx.is_empty() || self.peer_fin || self.error.is_some() || self.state == State::Closed
    }

    pub fn writable(&self) -> bool {
        self.error.is_some() || (matches!(self.state, State::Established | State::CloseWait) && !self.fin_pending && self.tx.len() < BUFFER)
    }

    pub fn established(&self) -> bool {
        !matches!(self.state, State::SynSent | State::SynRcvd | State::Closed)
    }

    pub fn rx_len(&self) -> usize {
        self.rx.len()
    }

    /// Queues data for sending. Returns how much was taken (0: buffer full).
    pub fn send(&mut self, data: &[u8], now: u64) -> KResult<usize> {
        if let Some(e) = self.error {
            return Err(e);
        }
        match self.state {
            State::Established | State::CloseWait | State::SynSent | State::SynRcvd if !self.fin_pending => {}
            _ => return Err(EPIPE),
        }
        let n = data.len().min(BUFFER - self.tx.len());
        self.tx.extend(&data[..n]);
        self.output(now);
        Ok(n)
    }

    /// Takes received data. Ok(0) means end of stream.
    pub fn recv(&mut self, buf: &mut [u8], now: u64) -> KResult<usize> {
        let n = buf.len().min(self.rx.len());
        for (i, b) in self.rx.drain(..n).enumerate() {
            buf[i] = b;
        }
        if n == 0 {
            if let Some(e) = self.error {
                return Err(e);
            }
        }
        // Tell the peer the window opened up (after it had become small).
        if n > 0 && (self.window() as u32).saturating_sub(self.adv_wnd) >= self.peer_mss as u32 * 2 && self.established() {
            self.ack_now = true;
            self.output(now);
        }
        Ok(n)
    }

    /// Orderly close of our sending direction: FIN after the queued data.
    pub fn close(&mut self, now: u64) {
        match self.state {
            State::SynSent | State::Closed => self.state = State::Closed,
            _ => {
                self.fin_pending = true;
                self.output(now);
            }
        }
    }

    /// Abort: RST to the peer, forget everything.
    pub fn abort(&mut self) {
        if !matches!(self.state, State::Closed | State::SynSent | State::TimeWait) {
            let seq = self.snd_nxt;
            self.emit(seq, RST | ACK, Vec::new(), None);
        }
        self.state = State::Closed;
    }

    /// Transmits what the windows allow: data, then FIN, else a pending ACK.
    fn output(&mut self, now: u64) {
        if !matches!(self.state, State::Established | State::CloseWait | State::FinWait1 | State::Closing | State::LastAck) {
            if self.ack_now && !matches!(self.state, State::Closed | State::SynSent) {
                let seq = self.snd_nxt;
                self.emit(seq, ACK, Vec::new(), None);
            }
            return;
        }
        let mss = self.peer_mss as usize;
        loop {
            let sent = self.snd_nxt.wrapping_sub(self.tx_seq) as usize;
            if sent >= self.tx.len() || (self.fin_sent && le(self.fin_seq, self.snd_nxt) && sent >= self.tx.len()) {
                break;
            }
            let in_flight = self.snd_nxt.wrapping_sub(self.snd_una) as usize;
            let wnd = self.snd_wnd as usize;
            if in_flight >= wnd {
                break;
            }
            let n = (self.tx.len() - sent).min(mss).min(wnd - in_flight);
            let (a, b) = self.tx.as_slices();
            let mut payload = Vec::with_capacity(n);
            if sent < a.len() {
                let end = (sent + n).min(a.len());
                payload.extend_from_slice(&a[sent..end]);
                payload.extend_from_slice(&b[..n - (end - sent)]);
            } else {
                payload.extend_from_slice(&b[sent - a.len()..sent - a.len() + n]);
            }
            let seq = self.snd_nxt;
            let flags = if sent + n == self.tx.len() { ACK | PSH } else { ACK };
            self.emit(seq, flags, payload, None);
            if self.rtt_probe.is_none() {
                self.rtt_probe = Some((seq.wrapping_add(n as u32), now));
            }
            self.snd_nxt = self.snd_nxt.wrapping_add(n as u32);
            if self.rtx_at.is_none() {
                self.rtx_at = Some(now + self.rto);
            }
        }
        let all_sent = self.snd_nxt.wrapping_sub(self.tx_seq) as usize >= self.tx.len();
        if self.fin_pending && !self.fin_sent && all_sent {
            self.fin_seq = self.snd_nxt;
            let seq = self.snd_nxt;
            self.emit(seq, FIN | ACK, Vec::new(), None);
            self.snd_nxt = self.snd_nxt.wrapping_add(1);
            self.fin_sent = true;
            self.state = match self.state {
                State::Established => State::FinWait1,
                State::CloseWait => State::LastAck,
                s => s,
            };
            if self.rtx_at.is_none() {
                self.rtx_at = Some(now + self.rto);
            }
        }
        if self.ack_now {
            let seq = self.snd_nxt;
            self.emit(seq, ACK, Vec::new(), None);
        }
    }

    fn rtt_sample(&mut self, r: u64) {
        match self.srtt {
            None => {
                self.srtt = Some(r);
                self.rttvar = r / 2;
            }
            Some(s) => {
                let diff = s.abs_diff(r);
                self.rttvar = (3 * self.rttvar + diff) / 4;
                self.srtt = Some((7 * s + r) / 8);
            }
        }
        self.rto = (self.srtt.unwrap() + (4 * self.rttvar).max(MS)).clamp(RTO_MIN, RTO_MAX);
    }

    fn acceptable(&self, seg: &Segment) -> bool {
        let wnd = self.window() as u32;
        let len = seg.seq_len();
        let end = self.rcv_nxt.wrapping_add(wnd);
        let in_wnd = |s: u32| le(self.rcv_nxt, s) && lt(s, end);
        match (len, wnd) {
            (0, 0) => seg.seq == self.rcv_nxt,
            (0, _) => in_wnd(seg.seq),
            (_, 0) => false,
            _ => in_wnd(seg.seq) || in_wnd(seg.seq.wrapping_add(len - 1)),
        }
    }

    fn enter_time_wait(&mut self, now: u64) {
        self.state = State::TimeWait;
        self.time_wait_end = now + TIME_WAIT;
        self.rtx_at = None;
    }

    /// Processes an incoming segment for this connection.
    pub fn input(&mut self, seg: &Segment, now: u64) {
        if self.state == State::Closed {
            return;
        }
        if self.state == State::SynSent {
            self.input_syn_sent(seg, now);
            return;
        }
        if !self.acceptable(seg) {
            if seg.flags & RST == 0 {
                self.ack_now = true; // e.g. a retransmission of something we already have
                self.output(now);
            }
            return;
        }
        if seg.flags & RST != 0 {
            self.error = Some(if self.state == State::SynRcvd { ECONNREFUSED } else { ECONNRESET });
            self.state = State::Closed;
            return;
        }
        if seg.flags & SYN != 0 {
            if self.state == State::SynRcvd && seg.seq.wrapping_add(1) == self.rcv_nxt {
                self.send_syn(now); // our SYN-ACK was lost: the peer retransmitted its SYN
            } else {
                self.ack_now = true;
                self.output(now);
            }
            return;
        }
        if seg.flags & ACK == 0 {
            return;
        }
        if self.state == State::SynRcvd {
            if lt(self.snd_una, seg.ack) && le(seg.ack, self.snd_nxt) {
                self.state = State::Established;
                self.snd_wnd = seg.window as u32;
                self.snd_wl1 = seg.seq;
                self.snd_wl2 = seg.ack;
            } else {
                self.emit(seg.ack, RST, Vec::new(), None);
                return;
            }
        }
        self.process_ack(seg, now);
        if self.state == State::Closed {
            return;
        }
        self.process_data(seg, now);
        self.output(now);
    }

    fn input_syn_sent(&mut self, seg: &Segment, now: u64) {
        let ack_ok = seg.flags & ACK != 0 && lt(self.iss, seg.ack) && le(seg.ack, self.snd_nxt);
        if seg.flags & ACK != 0 && !ack_ok {
            if seg.flags & RST == 0 {
                self.emit(seg.ack, RST, Vec::new(), None);
            }
            return;
        }
        if seg.flags & RST != 0 {
            if ack_ok {
                self.error = Some(ECONNREFUSED);
                self.state = State::Closed;
            }
            return;
        }
        if seg.flags & SYN == 0 {
            return;
        }
        self.rcv_nxt = seg.seq.wrapping_add(1);
        self.peer_mss = seg.mss.unwrap_or(DEFAULT_MSS).clamp(64, OUR_MSS);
        if ack_ok {
            self.snd_una = seg.ack;
            self.snd_wnd = seg.window as u32;
            self.snd_wl1 = seg.seq;
            self.snd_wl2 = seg.ack;
            self.state = State::Established;
            self.rtx_at = None;
            self.retries = 0;
            self.ack_now = true;
            self.output(now);
        } else {
            // Simultaneous open.
            self.state = State::SynRcvd;
            self.rtx_at = None;
            self.send_syn(now);
        }
    }

    fn process_ack(&mut self, seg: &Segment, now: u64) {
        if lt(self.snd_nxt, seg.ack) {
            self.ack_now = true; // acknowledges something never sent
            return;
        }
        let in_flight = self.snd_nxt.wrapping_sub(self.snd_una);
        if lt(self.snd_una, seg.ack) {
            // New data acknowledged.
            let data_acked = seg.ack.wrapping_sub(self.tx_seq).min(self.tx.len() as u32) as usize;
            if lt(self.tx_seq, seg.ack) {
                self.tx.drain(..data_acked);
                self.tx_seq = self.tx_seq.wrapping_add(data_acked as u32);
            }
            self.snd_una = seg.ack;
            self.retries = 0;
            self.dup_acks = 0;
            if let Some((probe, sent_at)) = self.rtt_probe {
                if le(probe, seg.ack) {
                    self.rtt_sample(now.saturating_sub(sent_at));
                    self.rtt_probe = None;
                }
            }
            self.rtx_at = if self.snd_una == self.snd_nxt { None } else { Some(now + self.rto) };
            if self.fin_sent && lt(self.fin_seq, seg.ack) {
                self.fin_acked = true;
            }
        } else if seg.ack == self.snd_una && seg.payload.is_empty() && seg.flags & FIN == 0 && in_flight > 0 && seg.window as u32 == self.snd_wnd {
            self.dup_acks += 1;
            if self.dup_acks == 3 {
                // Fast retransmit: resend from the first unacknowledged byte.
                self.go_back();
                self.retransmits += 1;
            }
        }
        // Window update (RFC 9293 3.10.7.4).
        if lt(self.snd_wl1, seg.seq) || (self.snd_wl1 == seg.seq && le(self.snd_wl2, seg.ack)) {
            self.snd_wnd = seg.window as u32;
            self.snd_wl1 = seg.seq;
            self.snd_wl2 = seg.ack;
        }
        if self.fin_acked {
            match self.state {
                State::FinWait1 => self.state = State::FinWait2,
                State::Closing => self.enter_time_wait(now),
                State::LastAck => {
                    self.state = State::Closed;
                    self.rtx_at = None;
                }
                _ => {}
            }
        }
    }

    fn process_data(&mut self, seg: &Segment, now: u64) {
        let can_receive = matches!(self.state, State::Established | State::FinWait1 | State::FinWait2);
        if can_receive && !seg.payload.is_empty() {
            let mut seq = seg.seq;
            let mut data: &[u8] = &seg.payload;
            if lt(seq, self.rcv_nxt) {
                let skip = self.rcv_nxt.wrapping_sub(seq) as usize;
                data = if skip < data.len() { &data[skip..] } else { &[] };
                seq = self.rcv_nxt;
            }
            if !data.is_empty() {
                if seq == self.rcv_nxt && self.ooo.is_empty() {
                    let room = BUFFER - self.rx.len();
                    let n = data.len().min(room);
                    self.rx.extend(&data[..n]);
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(n as u32);
                    self.unacked_segs += 1;
                    if n < data.len() || self.unacked_segs >= 2 {
                        self.ack_now = true;
                    } else if self.ack_at.is_none() {
                        self.ack_at = Some(now + DELACK);
                    }
                } else if seq == self.rcv_nxt {
                    let room = BUFFER - self.rx.len();
                    let n = data.len().min(room);
                    self.rx.extend(&data[..n]);
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(n as u32);
                    self.drain_ooo();
                    self.ack_now = true; // a hole was filled: tell the sender at once
                } else {
                    if self.ooo.len() < 64 {
                        self.ooo.entry(seq).or_insert_with(|| data.to_vec());
                    }
                    self.ack_now = true; // out of order: duplicate ACK for fast retransmit
                }
            } else {
                self.ack_now = true; // old data: the sender missed our ACK
            }
        }
        if seg.flags & FIN != 0 && !self.peer_fin && seg.seq.wrapping_add(seg.payload.len() as u32) == self.rcv_nxt {
            self.peer_fin = true;
            self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            self.ack_now = true;
            match self.state {
                State::Established => self.state = State::CloseWait,
                State::FinWait1 => {
                    if self.fin_acked {
                        self.enter_time_wait(now);
                    } else {
                        self.state = State::Closing;
                    }
                }
                State::FinWait2 => self.enter_time_wait(now),
                _ => {}
            }
        } else if seg.flags & FIN != 0 && self.state == State::TimeWait {
            self.ack_now = true; // our last ACK was lost
            self.time_wait_end = now + TIME_WAIT;
        }
    }

    fn drain_ooo(&mut self) {
        while let Some((&seq, _)) = self.ooo.iter().next() {
            if lt(self.rcv_nxt, seq) {
                break;
            }
            let data = self.ooo.remove(&seq).unwrap();
            let skip = self.rcv_nxt.wrapping_sub(seq) as usize;
            if skip < data.len() {
                let rest = &data[skip..];
                let n = rest.len().min(BUFFER - self.rx.len());
                self.rx.extend(&rest[..n]);
                self.rcv_nxt = self.rcv_nxt.wrapping_add(n as u32);
            }
        }
    }

    /// Restart sending from snd_una (after a timeout or three duplicate ACKs).
    fn go_back(&mut self) {
        self.snd_nxt = self.snd_una;
        if self.fin_sent && !self.fin_acked {
            self.fin_sent = false;
        }
        self.rtt_probe = None;
    }

    /// Runs the timers: retransmission (with backoff), zero-window probes, TIME_WAIT expiry.
    pub fn on_timer(&mut self, now: u64) {
        if self.state == State::TimeWait {
            if now >= self.time_wait_end {
                self.state = State::Closed;
            }
            return;
        }
        if self.ack_at.is_some_and(|t| now >= t) {
            self.ack_now = true;
            self.output(now);
        }
        let Some(at) = self.rtx_at else { return };
        if now < at {
            return;
        }
        if self.retries >= MAX_RETRIES {
            self.error = Some(ETIMEDOUT);
            self.abort();
            return;
        }
        self.retries += 1;
        self.retransmits += 1;
        self.timeouts += 1;
        self.rto = (self.rto * 2).min(RTO_MAX);
        self.rtx_at = Some(now + self.rto);
        match self.state {
            State::SynSent | State::SynRcvd => self.send_syn(now),
            _ => {
                self.go_back();
                if self.snd_wnd == 0 && !self.tx.is_empty() {
                    // Zero window: probe with one byte.
                    let seq = self.snd_nxt;
                    let b = alloc::vec![self.tx[seq.wrapping_sub(self.tx_seq) as usize]];
                    self.emit(seq, ACK, b, None);
                    self.snd_nxt = self.snd_nxt.wrapping_add(1);
                } else {
                    self.output(now);
                }
            }
        }
    }
}

/// A reset for a segment that matches no connection (RFC 9293 3.10.7.1).
pub fn reset_for(seg: &Segment) -> Option<Segment> {
    if seg.flags & RST != 0 {
        return None;
    }
    Some(if seg.flags & ACK != 0 {
        Segment { sport: seg.dport, dport: seg.sport, seq: seg.ack, flags: RST, ..Default::default() }
    } else {
        Segment { sport: seg.dport, dport: seg.sport, seq: 0, ack: seg.seq.wrapping_add(seg.seq_len()), flags: RST | ACK, ..Default::default() }
    })
}

#[cfg(test)]
mod tests;
