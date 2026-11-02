// SPDX-License-Identifier: Apache-2.0
//! Ingress packet filter (ADR-0030 S-17).
//!
//! Every received Ethernet frame is judged *before* the IP stack sees it. The policy is
//! default-deny for anything that starts a conversation with us: only ARP, answers to our own
//! requests (DHCP, DNS, TCP replies, ICMP errors/echo replies) and explicitly opened TCP listen
//! ports get through. Malformed, spoofed and unusual packets (IP options, fragments, martian
//! sources, redirects, source routing) are dropped silently so a scan learns nothing.
//!
//! Pure and allocation-light so every rule is unit-tested; fed with bounds-checked readers only.

use alloc::vec::Vec;
use ksparse::bytes::{array_at, slice_at, u16_at, u8_at};

pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETHERTYPE_IPV6: u16 = 0x86DD;
const IP6_HOP_BY_HOP: u8 = 0;
const IP6_ICMPV6: u8 = 58;
const IP_ICMP: u8 = 1;
const IP_TCP: u8 = 6;
const IP_UDP: u8 = 17;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Runt,
    BadHeader,
    Fragment,
    IpOptions,
    MartianSource,
    NotForUs,
    UnsolicitedUdp,
    UnsolicitedTcp,
    IcmpType,
    Ethernet,
    Arp,
    Protocol,
    NoAddress,
    Ipv6Header,
    Ipv6ExtensionHeader,
    Ipv6Source,
    NdpHopLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Drop(Reason),
    /// An allowed echo request: the caller answers it (rate-limited), the stack never sees it.
    EchoRequest,
}

/// What the filter knows about this host.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub addr: Option<[u8; 4]>,
    pub prefix: u8,
    pub tcp_listen: Vec<u16>,
    /// Answer ICMP echo requests from the local subnet (default off).
    pub allow_echo: bool,
    /// Our IPv6 addresses (link-local and SLAAC).
    pub addr6: Vec<[u8; 16]>,
}

fn in_subnet(addr: [u8; 4], net: [u8; 4], prefix: u8) -> bool {
    let (a, n) = (u32::from_be_bytes(addr), u32::from_be_bytes(net));
    let mask = u32::MAX
        .checked_shl(32u32.saturating_sub(u32::from(prefix)))
        .unwrap_or(0);
    a & mask == n & mask
}

fn martian_source(src: [u8; 4]) -> bool {
    let first = src[0];
    src == [0, 0, 0, 0] || first == 127 || first >= 224 // loopback, multicast, reserved, broadcast
}

/// Judges one Ethernet frame.
pub fn ingress(frame: &[u8], cfg: &Config) -> Verdict {
    let (Some(src_mac), Some(ethertype)) = (array_at::<6>(frame, 6), u16_at(frame, 12)) else {
        return Verdict::Drop(Reason::Runt);
    };
    let ethertype = ethertype.swap_bytes(); // wire order is big-endian
    if src_mac[0] & 1 != 0 {
        return Verdict::Drop(Reason::Ethernet); // multicast/broadcast source
    }
    match ethertype {
        ETHERTYPE_ARP => arp(frame, src_mac),
        ETHERTYPE_IPV4 => ipv4(frame.get(14..).unwrap_or(&[]), cfg),
        ETHERTYPE_IPV6 => ipv6(frame.get(14..).unwrap_or(&[]), cfg),
        _ => Verdict::Drop(Reason::Protocol),
    }
}

fn arp(frame: &[u8], src_mac: [u8; 6]) -> Verdict {
    let p = frame.get(14..).unwrap_or(&[]);
    // Ethernet/IPv4 only; the claimed sender hardware address must be the frame's sender.
    let ok = u16_at(p, 0) == Some(0x0100)
        && u16_at(p, 2) == Some(0x0008)
        && u8_at(p, 4) == Some(6)
        && u8_at(p, 5) == Some(4)
        && array_at::<6>(p, 8) == Some(src_mac);
    if ok {
        Verdict::Pass
    } else {
        Verdict::Drop(Reason::Arp)
    }
}

fn ipv4(p: &[u8], cfg: &Config) -> Verdict {
    let (Some(vihl), Some(total), Some(frag), Some(proto)) =
        (u8_at(p, 0), u16_at(p, 2), u16_at(p, 6), u8_at(p, 9))
    else {
        return Verdict::Drop(Reason::Runt);
    };
    let total = usize::from(total.swap_bytes());
    if vihl >> 4 != 4 || total < 20 || total > p.len() {
        return Verdict::Drop(Reason::BadHeader);
    }
    if vihl & 0x0F != 5 {
        return Verdict::Drop(Reason::IpOptions); // source routing & friends
    }
    let frag = frag.swap_bytes();
    if frag & 0x3FFF != 0 {
        return Verdict::Drop(Reason::Fragment); // MF set or non-zero offset
    }
    if u8_at(p, 8) == Some(0) {
        return Verdict::Drop(Reason::BadHeader); // TTL 0
    }
    let (Some(src), Some(dst)) = (array_at::<4>(p, 12), array_at::<4>(p, 16)) else {
        return Verdict::Drop(Reason::Runt);
    };
    if martian_source(src) {
        // The one legitimate unspecified source is a DHCP client; we never serve DHCP.
        return Verdict::Drop(Reason::MartianSource);
    }
    let Some(body) = slice_at(p, 20, total.saturating_sub(20)) else {
        return Verdict::Drop(Reason::BadHeader);
    };
    match cfg.addr {
        None => {
            // Not configured yet: only the DHCP server's reply (UDP 67 -> 68) may arrive.
            if proto == IP_UDP
                && u16_at(body, 0) == Some(67u16.swap_bytes())
                && u16_at(body, 2) == Some(68u16.swap_bytes())
            {
                Verdict::Pass
            } else {
                Verdict::Drop(Reason::NoAddress)
            }
        }
        Some(me) => {
            if src == me {
                return Verdict::Drop(Reason::MartianSource); // spoofed as ourselves (LAND)
            }
            let subnet_broadcast = {
                let host_bits = 32u32.saturating_sub(u32::from(cfg.prefix));
                let m = u32::from_be_bytes(me)
                    | u32::MAX
                        .checked_shr(32u32.saturating_sub(host_bits))
                        .unwrap_or(0);
                host_bits != 0 && dst == m.to_be_bytes()
            };
            let to_us = dst == me || dst == [255, 255, 255, 255] || subnet_broadcast;
            if !to_us {
                return Verdict::Drop(Reason::NotForUs);
            }
            let unicast = dst == me;
            match proto {
                IP_ICMP => icmp(body, src, me, cfg, unicast),
                IP_UDP => udp(body),
                IP_TCP => tcp(body, cfg, unicast),
                _ => Verdict::Drop(Reason::Protocol),
            }
        }
    }
}

fn icmp(body: &[u8], src: [u8; 4], me: [u8; 4], cfg: &Config, unicast: bool) -> Verdict {
    match u8_at(body, 0) {
        Some(0) | Some(3) | Some(11) | Some(12) => Verdict::Pass, // echo reply, unreachable, time exceeded, parameter problem
        Some(8) if cfg.allow_echo && unicast && in_subnet(src, me, cfg.prefix) => {
            Verdict::EchoRequest
        }
        _ => Verdict::Drop(Reason::IcmpType), // redirects, router adverts, timestamp, unsolicited echo, ...
    }
}

fn udp(body: &[u8]) -> Verdict {
    let (Some(sport), Some(dport)) = (u16_at(body, 0), u16_at(body, 2)) else {
        return Verdict::Drop(Reason::Runt);
    };
    let (sport, dport) = (sport.swap_bytes(), dport.swap_bytes());
    // Only replies to our own sockets: DHCP answers and DNS answers (smoltcp picks random
    // source ports above 1024 for queries). Anything else UDP is unsolicited.
    if (sport == 67 && dport == 68) || (sport == 53 && dport > 1024) {
        Verdict::Pass
    } else {
        Verdict::Drop(Reason::UnsolicitedUdp)
    }
}

fn tcp(body: &[u8], cfg: &Config, unicast: bool) -> Verdict {
    let (Some(dport), Some(flags)) = (u16_at(body, 2), u8_at(body, 13)) else {
        return Verdict::Drop(Reason::Runt);
    };
    if !unicast {
        return Verdict::Drop(Reason::NotForUs);
    }
    let dport = dport.swap_bytes();
    let (syn, ack) = (flags & 0x02 != 0, flags & 0x10 != 0);
    if syn && !ack && !cfg.tcp_listen.contains(&dport) {
        return Verdict::Drop(Reason::UnsolicitedTcp); // a closed port stays silent
    }
    Verdict::Pass
}

fn is_link_local6(a: &[u8; 16]) -> bool {
    a[0] == 0xFE && a[1] & 0xC0 == 0x80
}

/// Whether `dst` is addressed to this host: one of our unicast addresses, all-nodes, or the
/// solicited-node multicast group of one of our addresses.
fn for_us6(dst: &[u8; 16], cfg: &Config) -> bool {
    if cfg.addr6.contains(dst) {
        return true;
    }
    if dst[..] == [0xFF, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1] {
        return true;
    }
    let solicited = dst[..13] == [0xFF, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xFF];
    solicited && cfg.addr6.iter().any(|a| a[13..] == dst[13..])
}

fn ipv6(p: &[u8], cfg: &Config) -> Verdict {
    let (Some(vtf), Some(plen), Some(mut next), Some(hop)) =
        (u8_at(p, 0), u16_at(p, 4), u8_at(p, 6), u8_at(p, 7))
    else {
        return Verdict::Drop(Reason::Runt);
    };
    let plen = usize::from(plen.swap_bytes());
    if vtf >> 4 != 6 || hop == 0 || plen > p.len().saturating_sub(40) {
        return Verdict::Drop(Reason::Ipv6Header);
    }
    let (Some(src), Some(dst)) = (array_at::<16>(p, 8), array_at::<16>(p, 24)) else {
        return Verdict::Drop(Reason::Runt);
    };
    let mut body = slice_at(p, 40, plen).unwrap_or(&[]);
    // Extension headers: only a hop-by-hop header in front of ICMPv6 (MLD) is tolerated;
    // routing (RH0), fragment, destination-options chains are dropped.
    if next == IP6_HOP_BY_HOP {
        let inner = u8_at(body, 0);
        let hl = usize::from(u8_at(body, 1).unwrap_or(0))
            .saturating_add(1)
            .saturating_mul(8);
        if inner != Some(IP6_ICMPV6) {
            return Verdict::Drop(Reason::Ipv6ExtensionHeader);
        }
        body = body.get(hl..).unwrap_or(&[]);
        next = IP6_ICMPV6;
    }
    let unspecified = src == [0u8; 16];
    let loopback = src[..15] == [0u8; 15] && src[15] == 1;
    if (src[0] == 0xFF) || loopback || cfg.addr6.contains(&src) {
        return Verdict::Drop(Reason::Ipv6Source);
    }
    if !for_us6(&dst, cfg) {
        return Verdict::Drop(Reason::NotForUs);
    }
    match next {
        IP6_ICMPV6 => {
            let kind = u8_at(body, 0).unwrap_or(0);
            match kind {
                1..=4 | 129 => Verdict::Pass, // errors, echo reply
                130..=132 if is_link_local6(&src) && hop == 1 => Verdict::Pass, // MLD
                134..=136 => {
                    // Neighbour discovery is only valid with hop limit 255 (not routed in), and an
                    // unspecified source is only legal for duplicate-address-detection solicitations.
                    if hop != 255 {
                        Verdict::Drop(Reason::NdpHopLimit)
                    } else if (unspecified && kind != 135) || (kind == 134 && !is_link_local6(&src))
                    {
                        Verdict::Drop(Reason::Ipv6Source)
                    } else {
                        Verdict::Pass
                    }
                }
                _ => Verdict::Drop(Reason::IcmpType), // redirects, router solicitations, echo requests
            }
        }
        _ if unspecified => Verdict::Drop(Reason::Ipv6Source),
        IP_UDP => udp(body),
        IP_TCP => tcp(body, cfg, true),
        _ => Verdict::Drop(Reason::Protocol),
    }
}

/// Builds the echo reply for an `EchoRequest` frame (swapped addresses, type 0, fresh checksums).
pub fn echo_reply(frame: &[u8]) -> Option<Vec<u8>> {
    let mut out = frame.to_vec();
    let ihl_total = usize::from(u16_at(frame, 16)?.swap_bytes()).checked_add(14)?;
    let out_len = ihl_total.min(frame.len());
    out.truncate(out_len);
    // Swap MACs.
    let (dst, src) = (array_at::<6>(frame, 0)?, array_at::<6>(frame, 6)?);
    out.get_mut(0..6)?.copy_from_slice(&src);
    out.get_mut(6..12)?.copy_from_slice(&dst);
    // Swap IPs, fresh TTL, recompute header checksum.
    let (ip_src, ip_dst) = (array_at::<4>(frame, 26)?, array_at::<4>(frame, 30)?);
    out.get_mut(26..30)?.copy_from_slice(&ip_dst);
    out.get_mut(30..34)?.copy_from_slice(&ip_src);
    *out.get_mut(22)? = 64;
    out.get_mut(24..26)?.copy_from_slice(&[0, 0]);
    let hc = checksum(out.get(14..34)?);
    out.get_mut(24..26)?.copy_from_slice(&hc.to_be_bytes());
    // ICMP: type 8 -> 0, recompute checksum over the ICMP message.
    *out.get_mut(34)? = 0;
    out.get_mut(36..38)?.copy_from_slice(&[0, 0]);
    let ic = checksum(out.get(34..)?);
    out.get_mut(36..38)?.copy_from_slice(&ic.to_be_bytes());
    Some(out)
}

/// RFC 1071 internet checksum.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in data.chunks(2) {
        let hi = u32::from(chunk.first().copied().unwrap_or(0));
        let lo = u32::from(chunk.get(1).copied().unwrap_or(0));
        sum = sum.saturating_add((hi << 8) | lo);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF).saturating_add(sum >> 16);
    }
    !(sum as u16)
}

/// Token bucket: `rate` events per second, burst of `rate`.
#[derive(Clone, Debug)]
pub struct RateLimit {
    rate: u32,
    tokens: u32,
    last_ms: i64,
}

impl RateLimit {
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            tokens: rate,
            last_ms: 0,
        }
    }

    pub fn allow(&mut self, now_ms: i64) -> bool {
        let elapsed = now_ms.saturating_sub(self.last_ms);
        if elapsed >= 1000 {
            self.tokens = self.rate;
            self.last_ms = now_ms;
        }
        if self.tokens > 0 {
            self.tokens = self.tokens.saturating_sub(1);
            true
        } else {
            false
        }
    }
}
