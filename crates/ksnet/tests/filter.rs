// SPDX-License-Identifier: Apache-2.0
//! Ingress filter rules: default-deny, stealth for closed ports, no spoofed/martian/fragmented traffic.
use ksnet::filter::{checksum, echo_reply, ingress, Config, RateLimit, Reason, Verdict};

const ME: [u8; 4] = [10, 0, 2, 15];
const PEER: [u8; 4] = [10, 0, 2, 2];
const MAC_ME: [u8; 6] = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
const MAC_PEER: [u8; 6] = [0x52, 0x55, 0x0a, 0, 2, 2];

fn cfg() -> Config {
    Config {
        addr: Some(ME),
        prefix: 24,
        tcp_listen: vec![22],
        allow_echo: false,
        addr6: vec![],
    }
}

fn eth(src: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&MAC_ME);
    f.extend_from_slice(&src);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

fn ipv4(src: [u8; 4], dst: [u8; 4], proto: u8, body: &[u8]) -> Vec<u8> {
    ipv4_raw(0x45, 0, 64, src, dst, proto, body)
}

fn ipv4_raw(
    vihl: u8,
    frag: u16,
    ttl: u8,
    src: [u8; 4],
    dst: [u8; 4],
    proto: u8,
    body: &[u8],
) -> Vec<u8> {
    let mut p = vec![vihl, 0];
    p.extend_from_slice(&((20 + body.len()) as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(&frag.to_be_bytes());
    p.push(ttl);
    p.push(proto);
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(&src);
    p.extend_from_slice(&dst);
    p.extend_from_slice(body);
    eth(MAC_PEER, 0x0800, &p)
}

fn udp(sport: u16, dport: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&sport.to_be_bytes());
    b.extend_from_slice(&dport.to_be_bytes());
    b.extend_from_slice(&[0, 8, 0, 0]);
    b
}

fn tcp(dport: u16, flags: u8) -> Vec<u8> {
    let mut b = vec![0u8; 20];
    b[2..4].copy_from_slice(&dport.to_be_bytes());
    b[13] = flags;
    b
}

fn icmp(kind: u8) -> Vec<u8> {
    vec![kind, 0, 0, 0, 0, 1, 0, 1, b'k', b's']
}

#[test]
fn arp_is_allowed_only_when_well_formed_and_not_forged() {
    let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1];
    arp.extend_from_slice(&MAC_PEER);
    arp.extend_from_slice(&PEER);
    arp.extend_from_slice(&[0; 6]);
    arp.extend_from_slice(&ME);
    assert_eq!(ingress(&eth(MAC_PEER, 0x0806, &arp), &cfg()), Verdict::Pass);
    // Sender hardware address does not match the Ethernet source: spoofing attempt.
    assert_eq!(
        ingress(&eth([9; 6].map(|x: u8| x & 0xFE), 0x0806, &arp), &cfg()),
        Verdict::Drop(Reason::Arp)
    );
    // Broadcast/multicast source MAC.
    assert_eq!(
        ingress(&eth([0xFF; 6], 0x0806, &arp), &cfg()),
        Verdict::Drop(Reason::Ethernet)
    );
}

#[test]
fn runts_and_unknown_ethertypes_are_dropped() {
    assert_eq!(ingress(&[0; 5], &cfg()), Verdict::Drop(Reason::Runt));
    assert_eq!(
        ingress(&eth(MAC_PEER, 0x88CC, &[0; 40]), &cfg()),
        Verdict::Drop(Reason::Protocol)
    );
}

#[test]
fn malformed_and_unusual_ip_headers_are_dropped() {
    let b = udp(53, 50000);
    assert_eq!(
        ingress(&ipv4_raw(0x46, 0, 64, PEER, ME, 17, &b), &cfg()),
        Verdict::Drop(Reason::IpOptions)
    );
    assert_eq!(
        ingress(&ipv4_raw(0x45, 0x2000, 64, PEER, ME, 17, &b), &cfg()),
        Verdict::Drop(Reason::Fragment),
        "more-fragments"
    );
    assert_eq!(
        ingress(&ipv4_raw(0x45, 0x0010, 64, PEER, ME, 17, &b), &cfg()),
        Verdict::Drop(Reason::Fragment),
        "offset"
    );
    assert_eq!(
        ingress(&ipv4_raw(0x45, 0x4000, 0, PEER, ME, 17, &b), &cfg()),
        Verdict::Drop(Reason::BadHeader),
        "ttl 0"
    );
    assert_eq!(
        ingress(&ipv4_raw(0x65, 0, 64, PEER, ME, 17, &b), &cfg()),
        Verdict::Drop(Reason::BadHeader),
        "version 6 in an IPv4 frame"
    );
    let mut short = ipv4(PEER, ME, 17, &b);
    short.truncate(30);
    assert_eq!(
        ingress(&short, &cfg()),
        Verdict::Drop(Reason::BadHeader),
        "total length exceeds the frame"
    );
}

#[test]
fn spoofed_and_martian_sources_are_dropped() {
    let b = udp(53, 50000);
    for src in [
        ME,
        [127, 0, 0, 1],
        [0, 0, 0, 0],
        [224, 0, 0, 1],
        [255, 255, 255, 255],
        [240, 1, 2, 3],
    ] {
        assert_eq!(
            ingress(&ipv4(src, ME, 17, &b), &cfg()),
            Verdict::Drop(Reason::MartianSource),
            "{src:?}"
        );
    }
}

#[test]
fn only_traffic_addressed_to_us_passes() {
    let b = udp(53, 50000);
    assert_eq!(
        ingress(&ipv4(PEER, [10, 0, 2, 99], 17, &b), &cfg()),
        Verdict::Drop(Reason::NotForUs)
    );
    assert_eq!(ingress(&ipv4(PEER, ME, 17, &b), &cfg()), Verdict::Pass);
}

#[test]
fn udp_is_default_deny_except_dhcp_and_dns_answers() {
    assert_eq!(
        ingress(&ipv4(PEER, ME, 17, &udp(53, 40000)), &cfg()),
        Verdict::Pass
    );
    assert_eq!(
        ingress(&ipv4(PEER, [255; 4], 17, &udp(67, 68)), &cfg()),
        Verdict::Pass
    );
    for (s, d) in [
        (1234, 53),
        (53, 80),
        (5353, 5353),
        (137, 137),
        (1900, 1900),
        (4444, 50000),
    ] {
        assert_eq!(
            ingress(&ipv4(PEER, ME, 17, &udp(s, d)), &cfg()),
            Verdict::Drop(Reason::UnsolicitedUdp),
            "{s}->{d}"
        );
    }
}

#[test]
fn tcp_syn_to_closed_ports_is_silently_dropped_open_ports_and_replies_pass() {
    assert_eq!(
        ingress(&ipv4(PEER, ME, 6, &tcp(22, 0x02)), &cfg()),
        Verdict::Pass,
        "listening port"
    );
    assert_eq!(
        ingress(&ipv4(PEER, ME, 6, &tcp(23, 0x02)), &cfg()),
        Verdict::Drop(Reason::UnsolicitedTcp),
        "closed port: no RST"
    );
    assert_eq!(
        ingress(&ipv4(PEER, ME, 6, &tcp(50000, 0x12)), &cfg()),
        Verdict::Pass,
        "SYN+ACK answering our connect"
    );
    assert_eq!(
        ingress(&ipv4(PEER, ME, 6, &tcp(50000, 0x10)), &cfg()),
        Verdict::Pass,
        "established traffic"
    );
    assert_eq!(
        ingress(&ipv4(PEER, [255; 4], 6, &tcp(22, 0x02)), &cfg()),
        Verdict::Drop(Reason::NotForUs),
        "TCP to broadcast"
    );
}

#[test]
fn icmp_policy_echo_is_off_by_default_and_redirects_never_pass() {
    assert_eq!(
        ingress(&ipv4(PEER, ME, 1, &icmp(8)), &cfg()),
        Verdict::Drop(Reason::IcmpType)
    );
    let mut on = cfg();
    on.allow_echo = true;
    assert_eq!(
        ingress(&ipv4(PEER, ME, 1, &icmp(8)), &on),
        Verdict::EchoRequest
    );
    assert_eq!(
        ingress(&ipv4([192, 168, 9, 9], ME, 1, &icmp(8)), &on),
        Verdict::Drop(Reason::IcmpType),
        "off-subnet"
    );
    assert_eq!(
        ingress(&ipv4(PEER, [255; 4], 1, &icmp(8)), &on),
        Verdict::Drop(Reason::IcmpType),
        "no smurf amplification"
    );
    for t in [0u8, 3, 11, 12] {
        assert_eq!(
            ingress(&ipv4(PEER, ME, 1, &icmp(t)), &cfg()),
            Verdict::Pass,
            "type {t}"
        );
    }
    for t in [5u8, 9, 10, 13, 17] {
        assert_eq!(
            ingress(&ipv4(PEER, ME, 1, &icmp(t)), &on),
            Verdict::Drop(Reason::IcmpType),
            "type {t}"
        );
    }
}

#[test]
fn before_an_address_exists_only_the_dhcp_server_reply_gets_in() {
    let none = Config::default();
    assert_eq!(
        ingress(&ipv4(PEER, [255; 4], 17, &udp(67, 68)), &none),
        Verdict::Pass
    );
    assert_eq!(
        ingress(&ipv4(PEER, [255; 4], 17, &udp(53, 40000)), &none),
        Verdict::Drop(Reason::NoAddress)
    );
    assert_eq!(
        ingress(&ipv4(PEER, [255; 4], 6, &tcp(22, 2)), &none),
        Verdict::Drop(Reason::NoAddress)
    );
}

#[test]
fn echo_reply_is_a_valid_swapped_packet() {
    let mut on = cfg();
    on.allow_echo = true;
    let req = ipv4(PEER, ME, 1, &icmp(8));
    let reply = echo_reply(&req).unwrap();
    assert_eq!(&reply[0..6], &MAC_PEER);
    assert_eq!(&reply[6..12], &MAC_ME);
    assert_eq!(&reply[26..30], &ME);
    assert_eq!(&reply[30..34], &PEER);
    assert_eq!(reply[34], 0, "echo reply");
    assert_eq!(checksum(&reply[14..34]), 0, "IP header checksum verifies");
    assert_eq!(checksum(&reply[34..]), 0, "ICMP checksum verifies");
}

#[test]
fn rate_limit_allows_a_burst_then_refills_each_second() {
    let mut r = RateLimit::new(3);
    assert!([0, 1, 2].iter().all(|&t| r.allow(t)));
    assert!(!r.allow(3));
    assert!(!r.allow(999));
    assert!(r.allow(1000), "bucket refills after a second");
}

// ---- IPv6 ----

const ME6: [u8; 16] = [
    0xFE, 0x80, 0, 0, 0, 0, 0, 0, 0x50, 0x54, 0, 0xFF, 0xFE, 0x12, 0x34, 0x56,
];
const GW6: [u8; 16] = [
    0xFE, 0x80, 0, 0, 0, 0, 0, 0, 0x50, 0x55, 0x0A, 0xFF, 0xFE, 0, 2, 2,
];
const GLOBAL6: [u8; 16] = [
    0xFD, 0, 0, 0, 0, 0, 0, 0, 0x50, 0x54, 0, 0xFF, 0xFE, 0x12, 0x34, 0x56,
];
const ALL_NODES: [u8; 16] = [0xFF, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

fn cfg6() -> Config {
    Config {
        addr6: vec![ME6, GLOBAL6],
        ..Config::default()
    }
}

fn ipv6(src: [u8; 16], dst: [u8; 16], next: u8, hop: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![0x60, 0, 0, 0];
    p.extend_from_slice(&(body.len() as u16).to_be_bytes());
    p.push(next);
    p.push(hop);
    p.extend_from_slice(&src);
    p.extend_from_slice(&dst);
    p.extend_from_slice(body);
    eth(MAC_PEER, 0x86DD, &p)
}

fn icmp6(kind: u8) -> Vec<u8> {
    vec![kind, 0, 0, 0, 0, 0, 0, 0]
}

fn solicited_node(a: &[u8; 16]) -> [u8; 16] {
    let mut d = [0xFF, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xFF, 0, 0, 0];
    d[13..].copy_from_slice(&a[13..]);
    d
}

#[test]
fn ipv6_neighbour_discovery_needs_hop_limit_255_and_a_valid_source() {
    assert_eq!(
        ingress(&ipv6(GW6, ME6, 58, 255, &icmp6(136)), &cfg6()),
        Verdict::Pass,
        "neighbour advertisement"
    );
    assert_eq!(
        ingress(&ipv6(GW6, ME6, 58, 64, &icmp6(136)), &cfg6()),
        Verdict::Drop(Reason::NdpHopLimit),
        "routed-in NA"
    );
    assert_eq!(
        ingress(&ipv6(GW6, ALL_NODES, 58, 255, &icmp6(134)), &cfg6()),
        Verdict::Pass,
        "router advertisement from link-local"
    );
    let mut global_src = GW6;
    global_src[0] = 0x20;
    assert_eq!(
        ingress(&ipv6(global_src, ALL_NODES, 58, 255, &icmp6(134)), &cfg6()),
        Verdict::Drop(Reason::Ipv6Source),
        "RA must come from a link-local address"
    );
    // DAD from :: is legal only as a neighbour solicitation.
    assert_eq!(
        ingress(
            &ipv6([0; 16], solicited_node(&ME6), 58, 255, &icmp6(135)),
            &cfg6()
        ),
        Verdict::Pass
    );
    assert_eq!(
        ingress(&ipv6([0; 16], ALL_NODES, 58, 255, &icmp6(136)), &cfg6()),
        Verdict::Drop(Reason::Ipv6Source)
    );
}

#[test]
fn ipv6_redirects_echo_requests_and_extension_chains_are_dropped() {
    assert_eq!(
        ingress(&ipv6(GW6, ME6, 58, 255, &icmp6(137)), &cfg6()),
        Verdict::Drop(Reason::IcmpType),
        "redirect"
    );
    assert_eq!(
        ingress(&ipv6(GW6, ME6, 58, 64, &icmp6(128)), &cfg6()),
        Verdict::Drop(Reason::IcmpType),
        "echo request"
    );
    assert_eq!(
        ingress(&ipv6(GW6, ME6, 58, 64, &icmp6(129)), &cfg6()),
        Verdict::Pass,
        "echo reply"
    );
    for next in [43u8, 44, 60] {
        let mut body = vec![17u8, 0, 0, 0, 0, 0, 0, 0];
        body.extend_from_slice(&[0; 8]);
        assert_eq!(
            ingress(&ipv6(GW6, ME6, next, 64, &body), &cfg6()),
            Verdict::Drop(Reason::Protocol),
            "ext header {next}"
        );
    }
    // Hop-by-hop that does not wrap ICMPv6 (e.g. jumbo/other options carrying UDP) is refused.
    assert_eq!(
        ingress(&ipv6(GW6, ME6, 0, 64, &[17, 0, 0, 0, 0, 0, 0, 0]), &cfg6()),
        Verdict::Drop(Reason::Ipv6ExtensionHeader)
    );
}

#[test]
fn ipv6_destination_and_source_sanity() {
    let other = [0xFE, 0x80, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
    assert_eq!(
        ingress(&ipv6(GW6, other, 58, 255, &icmp6(136)), &cfg6()),
        Verdict::Drop(Reason::NotForUs)
    );
    assert_eq!(
        ingress(&ipv6(ME6, ME6, 58, 255, &icmp6(136)), &cfg6()),
        Verdict::Drop(Reason::Ipv6Source),
        "spoofed as ourselves"
    );
    assert_eq!(
        ingress(&ipv6(ALL_NODES, ME6, 58, 255, &icmp6(136)), &cfg6()),
        Verdict::Drop(Reason::Ipv6Source),
        "multicast source"
    );
    let mut lo = [0u8; 16];
    lo[15] = 1;
    assert_eq!(
        ingress(&ipv6(lo, ME6, 58, 255, &icmp6(136)), &cfg6()),
        Verdict::Drop(Reason::Ipv6Source),
        "loopback source"
    );
    assert_eq!(
        ingress(
            &ipv6(GW6, solicited_node(&GLOBAL6), 58, 255, &icmp6(135)),
            &cfg6()
        ),
        Verdict::Pass,
        "solicited-node group of a configured address"
    );
    assert_eq!(
        ingress(&ipv6(GW6, ME6, 58, 0, &icmp6(129)), &cfg6()),
        Verdict::Drop(Reason::Ipv6Header),
        "hop limit 0"
    );
}

#[test]
fn ipv6_udp_tcp_follow_the_same_default_deny() {
    let mut u = udp(53, 40000);
    u.extend_from_slice(&[]);
    assert_eq!(
        ingress(&ipv6(GW6, GLOBAL6, 17, 64, &u), &cfg6()),
        Verdict::Pass,
        "DNS answer"
    );
    assert_eq!(
        ingress(&ipv6(GW6, GLOBAL6, 17, 64, &udp(5353, 5353)), &cfg6()),
        Verdict::Drop(Reason::UnsolicitedUdp),
        "mDNS"
    );
    assert_eq!(
        ingress(&ipv6(GW6, GLOBAL6, 6, 64, &tcp(23, 0x02)), &cfg6()),
        Verdict::Drop(Reason::UnsolicitedTcp)
    );
}
