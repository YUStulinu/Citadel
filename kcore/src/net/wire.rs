//! Packet formats: Ethernet, ARP, IPv4, ICMP, UDP and TCP headers, and the Internet checksum.
use alloc::vec::Vec;

pub const ETH_IPV4: u16 = 0x0800;
pub const ETH_ARP: u16 = 0x0806;
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;

pub type Mac = [u8; 6];
pub const BROADCAST: Mac = [0xff; 6];

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// One's-complement sum of 16-bit words (RFC 1071), folded but not inverted.
pub fn sum(data: &[u8], mut acc: u32) -> u32 {
    let mut chunks = data.chunks_exact(2);
    for c in &mut chunks {
        acc += u16::from_be_bytes([c[0], c[1]]) as u32;
    }
    if let [last] = chunks.remainder() {
        acc += (*last as u32) << 8;
    }
    while acc >> 16 != 0 {
        acc = (acc & 0xffff) + (acc >> 16);
    }
    acc
}

pub fn checksum(data: &[u8]) -> u16 {
    !(sum(data, 0) as u16)
}

/// Checksum of a TCP or UDP segment with the IPv4 pseudo-header.
pub fn l4_checksum(src: u32, dst: u32, proto: u8, seg: &[u8]) -> u16 {
    let mut pseudo = [0u8; 12];
    pseudo[0..4].copy_from_slice(&src.to_be_bytes());
    pseudo[4..8].copy_from_slice(&dst.to_be_bytes());
    pseudo[9] = proto;
    pseudo[10..12].copy_from_slice(&(seg.len() as u16).to_be_bytes());
    !(sum(seg, sum(&pseudo, 0)) as u16)
}

pub struct Eth<'a> {
    pub dst: Mac,
    pub src: Mac,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

pub fn parse_eth(f: &[u8]) -> Option<Eth<'_>> {
    if f.len() < 14 {
        return None;
    }
    Some(Eth { dst: f[0..6].try_into().unwrap(), src: f[6..12].try_into().unwrap(), ethertype: be16(f, 12), payload: &f[14..] })
}

pub fn build_eth(dst: Mac, src: Mac, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(14 + payload.len());
    f.extend_from_slice(&dst);
    f.extend_from_slice(&src);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

pub struct Arp {
    pub op: u16, // 1 request, 2 reply
    pub sha: Mac,
    pub spa: u32,
    pub tha: Mac,
    pub tpa: u32,
}

pub fn parse_arp(p: &[u8]) -> Option<Arp> {
    if p.len() < 28 || be16(p, 0) != 1 || be16(p, 2) != ETH_IPV4 || p[4] != 6 || p[5] != 4 {
        return None;
    }
    Some(Arp { op: be16(p, 6), sha: p[8..14].try_into().unwrap(), spa: be32(p, 14), tha: p[18..24].try_into().unwrap(), tpa: be32(p, 24) })
}

pub fn build_arp(a: &Arp) -> Vec<u8> {
    let mut p = Vec::with_capacity(28);
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&ETH_IPV4.to_be_bytes());
    p.extend_from_slice(&[6, 4]);
    p.extend_from_slice(&a.op.to_be_bytes());
    p.extend_from_slice(&a.sha);
    p.extend_from_slice(&a.spa.to_be_bytes());
    p.extend_from_slice(&a.tha);
    p.extend_from_slice(&a.tpa.to_be_bytes());
    p
}

pub struct Ipv4<'a> {
    pub src: u32,
    pub dst: u32,
    pub proto: u8,
    pub ttl: u8,
    pub payload: &'a [u8],
}

/// Parses an IPv4 packet (no fragments, valid header checksum).
pub fn parse_ipv4(p: &[u8]) -> Option<Ipv4<'_>> {
    if p.len() < 20 || p[0] >> 4 != 4 {
        return None;
    }
    let ihl = (p[0] & 0xf) as usize * 4;
    let total = be16(p, 2) as usize;
    if ihl < 20 || total < ihl || total > p.len() || checksum(&p[..ihl]) != 0 {
        return None;
    }
    let frag = be16(p, 6);
    if frag & 0x3fff != 0 {
        return None; // fragments (MF set or an offset) are not reassembled
    }
    Some(Ipv4 { src: be32(p, 12), dst: be32(p, 16), proto: p[9], ttl: p[8], payload: &p[ihl..total] })
}

pub fn build_ipv4(src: u32, dst: u32, proto: u8, id: u16, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(20 + payload.len());
    p.extend_from_slice(&[0x45, 0]);
    p.extend_from_slice(&((20 + payload.len()) as u16).to_be_bytes());
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&0x4000u16.to_be_bytes()); // don't fragment
    p.extend_from_slice(&[64, proto, 0, 0]);
    p.extend_from_slice(&src.to_be_bytes());
    p.extend_from_slice(&dst.to_be_bytes());
    let c = checksum(&p[..20]);
    p[10..12].copy_from_slice(&c.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

pub struct Udp<'a> {
    pub sport: u16,
    pub dport: u16,
    pub payload: &'a [u8],
}

pub fn parse_udp(src: u32, dst: u32, p: &[u8]) -> Option<Udp<'_>> {
    if p.len() < 8 {
        return None;
    }
    let len = be16(p, 4) as usize;
    if len < 8 || len > p.len() {
        return None;
    }
    if be16(p, 6) != 0 && l4_checksum(src, dst, PROTO_UDP, &p[..len]) != 0 {
        return None;
    }
    Some(Udp { sport: be16(p, 0), dport: be16(p, 2), payload: &p[8..len] })
}

pub fn build_udp(src: u32, dst: u32, sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + payload.len());
    p.extend_from_slice(&sport.to_be_bytes());
    p.extend_from_slice(&dport.to_be_bytes());
    p.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(payload);
    let mut c = l4_checksum(src, dst, PROTO_UDP, &p);
    if c == 0 {
        c = 0xffff;
    }
    p[6..8].copy_from_slice(&c.to_be_bytes());
    p
}

pub const FIN: u8 = 0x01;
pub const SYN: u8 = 0x02;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;

/// A TCP segment (header fields that matter here, and the payload).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Segment {
    pub sport: u16,
    pub dport: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    pub mss: Option<u16>,
    pub payload: Vec<u8>,
}

impl Segment {
    /// Sequence space the segment occupies (SYN and FIN count as one each).
    pub fn seq_len(&self) -> u32 {
        self.payload.len() as u32 + (self.flags & SYN != 0) as u32 + (self.flags & FIN != 0) as u32
    }
}

pub fn parse_tcp(src: u32, dst: u32, p: &[u8]) -> Option<Segment> {
    if p.len() < 20 || l4_checksum(src, dst, PROTO_TCP, p) != 0 {
        return None;
    }
    let off = (p[12] >> 4) as usize * 4;
    if off < 20 || off > p.len() {
        return None;
    }
    let mut mss = None;
    let mut i = 20;
    while i < off {
        match p[i] {
            0 => break,
            1 => i += 1,
            k => {
                if i + 1 >= off {
                    break;
                }
                let l = p[i + 1] as usize;
                if l < 2 || i + l > off {
                    break;
                }
                if k == 2 && l == 4 {
                    mss = Some(be16(p, i + 2));
                }
                i += l;
            }
        }
    }
    Some(Segment { sport: be16(p, 0), dport: be16(p, 2), seq: be32(p, 4), ack: be32(p, 8), flags: p[13] & 0x3f, window: be16(p, 14), mss, payload: p[off..].to_vec() })
}

pub fn build_tcp(src: u32, dst: u32, s: &Segment) -> Vec<u8> {
    let opt_len = if s.mss.is_some() { 4 } else { 0 };
    let hlen = 20 + opt_len;
    let mut p = Vec::with_capacity(hlen + s.payload.len());
    p.extend_from_slice(&s.sport.to_be_bytes());
    p.extend_from_slice(&s.dport.to_be_bytes());
    p.extend_from_slice(&s.seq.to_be_bytes());
    p.extend_from_slice(&s.ack.to_be_bytes());
    p.push(((hlen / 4) as u8) << 4);
    p.push(s.flags);
    p.extend_from_slice(&s.window.to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 0]); // checksum, urgent pointer
    if let Some(m) = s.mss {
        p.extend_from_slice(&[2, 4]);
        p.extend_from_slice(&m.to_be_bytes());
    }
    p.extend_from_slice(&s.payload);
    let c = l4_checksum(src, dst, PROTO_TCP, &p);
    p[16..18].copy_from_slice(&c.to_be_bytes());
    p
}

/// ICMP echo request/reply: (type, id, seq, data).
pub fn parse_icmp_echo(p: &[u8]) -> Option<(u8, u16, u16, &[u8])> {
    if p.len() < 8 || checksum(p) != 0 {
        return None;
    }
    Some((p[0], be16(p, 4), be16(p, 6), &p[8..]))
}

pub fn build_icmp_echo(kind: u8, id: u16, seq: u16, data: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + data.len());
    p.extend_from_slice(&[kind, 0, 0, 0]);
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(data);
    let c = checksum(&p);
    p[2..4].copy_from_slice(&c.to_be_bytes());
    p
}

pub fn ip(a: u8, b: u8, c: u8, d: u8) -> u32 {
    u32::from_be_bytes([a, b, c, d])
}
