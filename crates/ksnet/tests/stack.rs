// SPDX-License-Identifier: Apache-2.0
//! End-to-end tests of the IP stack (DHCP, ping, DNS, TCP with packet loss, link loss) against a scripted server.
mod common;

use common::*;
use ksnet::stack::{Lease, PingResult, Stack, TcpState};
use ksnet::Nic;
use smoltcp::wire::{DhcpMessageType, Ipv4Address, Ipv4Cidr};

const CLIENT_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

struct World {
    client: Stack<WireNic>,
    server: Stack<ServerNic>,
    now: i64,
    up: std::rc::Rc<std::cell::Cell<bool>>,
}

fn world(drop_every: u32) -> World {
    let (mut a, b, up) = wire_pair(CLIENT_MAC, SERVER_MAC);
    a.drop_every = drop_every;
    let client = Stack::new(a, 0x1234_5678_9abc_def0, 0);
    let mut server = Stack::new(ServerNic::new(b), 0x0fed_cba9_8765_4321, 0);
    server.set_static(Lease {
        address: Ipv4Cidr::new(SERVER_IP, 24),
        router: None,
        dns: vec![],
    });
    World {
        client,
        server,
        now: 0,
        up,
    }
}

impl World {
    fn run(&mut self, ms: i64, mut until: impl FnMut(&mut World) -> bool) -> bool {
        let end = self.now + ms;
        while self.now < end {
            self.now += 1;
            self.client.poll(self.now);
            self.server.poll(self.now);
            if until(self) {
                return true;
            }
        }
        false
    }
    fn configure(&mut self) {
        assert!(
            self.run(10_000, |w| w.client.lease().is_some()),
            "no DHCP lease"
        );
    }
}

#[test]
fn dhcp_acquires_the_lease() {
    let mut w = world(0);
    w.configure();
    let l = w.client.lease().unwrap();
    assert_eq!(l.address, Ipv4Cidr::new(CLIENT_IP, 24));
    assert_eq!(l.router, Some(SERVER_IP));
    assert_eq!(l.dns, vec![DNS_IP]);
    let msgs = &w.server.nic().dhcp_msgs;
    assert_eq!(msgs.first(), Some(&DhcpMessageType::Discover));
    assert!(msgs.contains(&DhcpMessageType::Request));
}

#[test]
fn dhcp_survives_packet_loss() {
    let mut w = world(2); // every second client frame is lost
    assert!(
        w.run(60_000, |w| w.client.lease().is_some()),
        "no lease with 50% loss"
    );
}

#[test]
fn ping_the_gateway() {
    let mut w = world(0);
    w.configure();
    w.client.ping(SERVER_IP, 7).unwrap();
    let mut got = PingResult::Pending;
    assert!(w.run(5_000, |w| {
        got = w.client.ping_result();
        got != PingResult::Pending
    }));
    assert_eq!(
        got,
        PingResult::Reply {
            seq: 7,
            from: SERVER_IP
        }
    );
}

#[test]
fn dns_resolution() {
    let mut w = world(0);
    w.configure();
    w.client.resolve("example.org").unwrap();
    let mut res = None;
    assert!(w.run(10_000, |w| {
        res = w.client.resolve_result();
        res.is_some()
    }));
    assert_eq!(res, Some(Ok(NAME_IP)));
    assert!(w
        .server
        .nic()
        .dns_queries
        .iter()
        .any(|q| q == "example.org"));
}

#[test]
fn dns_failure_is_reported() {
    let mut w = world(0);
    w.configure();
    w.server.nic().answer_dns = false;
    w.client.resolve("nobody.invalid").unwrap();
    let mut res = None;
    assert!(w.run(60_000, |w| {
        res = w.client.resolve_result();
        res.is_some()
    }));
    assert_eq!(res, Some(Err(())));
}

#[test]
fn tcp_echo_roundtrip_with_loss() {
    let mut w = world(7);
    w.configure();
    let l = w.server.tcp_listen(7).unwrap();
    assert_eq!(w.server.tcp_state(l), TcpState::Listening);
    let c = w.client.tcp_connect(SERVER_IP, 7).unwrap();
    assert!(w.run(30_000, |w| w.client.tcp_state(c) == TcpState::Established));

    let payload: Vec<u8> = (0..20_000u32).map(|i| (i * 7 + 3) as u8).collect();
    let mut sent = 0;
    let mut echoed = Vec::new();
    let ok = w.run(120_000, |w| {
        if sent < payload.len() {
            sent += w.client.tcp_send(c, &payload[sent..]);
        }
        let mut buf = [0u8; 2048];
        let n = w.server.tcp_recv(l, &mut buf);
        if n > 0 {
            let mut off = 0;
            while off < n {
                off += w.server.tcp_send(l, &buf[off..n]);
            }
        }
        let n = w.client.tcp_recv(c, &mut buf);
        echoed.extend_from_slice(&buf[..n]);
        echoed.len() == payload.len()
    });
    assert!(ok, "echoed {} of {}", echoed.len(), payload.len());
    assert_eq!(echoed, payload);

    w.client.tcp_close(c);
    assert!(w.run(30_000, |w| w.server.tcp_state(l) == TcpState::PeerClosed));
    w.server.tcp_close(l);
    assert!(w.run(30_000, |w| w.client.tcp_state(c) == TcpState::Closed));
    w.client.tcp_release(c);
}

#[test]
fn link_loss_deconfigures_and_relearns() {
    let mut w = world(0);
    w.configure();
    let c = {
        let _ = w.server.tcp_listen(9).unwrap();
        w.client.tcp_connect(SERVER_IP, 9).unwrap()
    };
    assert!(w.run(10_000, |w| w.client.tcp_state(c) == TcpState::Established));
    w.up.set(false);
    w.run(5, |_| false);
    assert!(w.client.lease().is_none());
    assert_eq!(w.client.tcp_state(c), TcpState::Closed);
    assert!(w.client.ping(SERVER_IP, 1).is_err());
    w.up.set(true);
    w.configure();
    assert!(w.client.lease().is_some());
}

#[test]
fn operations_without_lease_are_refused() {
    let (a, _b, _) = wire_pair(CLIENT_MAC, SERVER_MAC);
    let mut s = Stack::new(a, 1, 0);
    assert!(s.tcp_connect(Ipv4Address::new(1, 2, 3, 4), 80).is_err());
    assert!(s.resolve("x.org").is_err());
    assert!(s.nic().link_up());
}
