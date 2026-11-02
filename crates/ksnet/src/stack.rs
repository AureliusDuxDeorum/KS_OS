// SPDX-License-Identifier: Apache-2.0
//! The IP stack: DHCPv4 client, DNS resolver, ICMP echo and TCP client sockets over smoltcp.
//!
//! Time is supplied by the caller in milliseconds (`now_ms`) so the stack is deterministic and
//! testable. Nothing here blocks: the kernel calls [`Stack::poll`] regularly and reads results.

use crate::nic::{Mac, Nic, NicDevice};
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::{dhcpv4, dns, icmp, tcp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    DnsQueryType, EthernetAddress, HardwareAddress, Icmpv4Packet, Icmpv4Repr, Icmpv6Packet,
    Icmpv6Repr, IpAddress, IpCidr, IpEndpoint, Ipv4Address, Ipv4Cidr, Ipv6Address, Ipv6Cidr,
};

const TCP_BUF: usize = 32 * 1024;
const MAX_TCP: usize = 8;
/// Frames processed per `poll` call.
pub const RX_BUDGET: u32 = 64;

/// The address configuration obtained from DHCP (or set statically).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub address: Ipv4Cidr,
    pub router: Option<Ipv4Address>,
    pub dns: Vec<Ipv4Address>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PingResult {
    /// Nothing to report yet.
    Pending,
    /// Echo reply received for this sequence number.
    Reply { seq: u16, from: Ipv4Address },
    /// Echo reply received over IPv6.
    Reply6 { seq: u16, from: Ipv6Address },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpId(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpState {
    Listening,
    Connecting,
    Established,
    /// The peer has closed its sending side; buffered data can still be read.
    PeerClosed,
    Closed,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Error {
    NoLease,
    NoSlot,
    Invalid,
    Busy,
}

pub struct Stack<N: Nic> {
    dev: NicDevice<N>,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp: Option<SocketHandle>,
    dns: SocketHandle,
    icmp: SocketHandle,
    tcp: [Option<SocketHandle>; MAX_TCP],
    lease: Option<Lease>,
    dns_query: Option<dns::QueryHandle>,
    ping_ident: u16,
    next_port: u16,
    last_link: bool,
}

/// Opaque 64-bit interface identifier derived from the per-boot seed (SplitMix64 finaliser).
fn link_local_iid(seed: u64, mac: &Mac) -> [u8; 8] {
    let m = mac.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b));
    let mut x = seed ^ m.rotate_left(24) ^ 0x9E37_79B9_7F4A_7C15;
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let mut iid = (x ^ (x >> 31)).to_be_bytes();
    if let Some(b) = iid.first_mut() {
        *b &= !0x02; // universal/local bit: locally administered
        *b &= !0x01; // unicast
    }
    iid
}

/// fe80::/10 (note that fec0::/10, the old site-local range, is *not* link-local).
fn is_link_local(a: &[u8; 16]) -> bool {
    a[0] == 0xFE && a[1] & 0xC0 == 0x80
}

fn instant(now_ms: i64) -> Instant {
    Instant::from_millis(now_ms)
}

impl<N: Nic> Stack<N> {
    /// `seed` should come from a hardware RNG: it drives TCP initial sequence numbers and ports.
    pub fn new(nic: N, seed: u64, now_ms: i64) -> Self {
        let mac = nic.mac();
        let mut dev = NicDevice::new(nic);
        let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        cfg.random_seed = seed;
        cfg.slaac = true; // IPv6 stateless autoconfiguration (all inbound traffic still goes through the filter)
        let mut iface = Interface::new(cfg, &mut dev, instant(now_ms));
        // smoltcp expects us to configure the IPv6 link-local address. The interface identifier is
        // an opaque per-boot value (not the MAC), so the address does not reveal the hardware.
        let iid = link_local_iid(seed, &mac);
        let mut ll = [0u8; 16];
        ll[0] = 0xFE;
        ll[1] = 0x80;
        ll[8..].copy_from_slice(&iid);
        iface.update_ip_addrs(|a| {
            let _ = a.push(IpCidr::Ipv6(Ipv6Cidr::new(Ipv6Address::from(ll), 64)));
        });
        let mut sockets = SocketSet::new(Vec::new());
        let dhcp = sockets.add(dhcpv4::Socket::new());
        let dns = sockets.add(dns::Socket::new(&[], vec![None, None, None, None]));
        let icmp = sockets.add(icmp::Socket::new(
            icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 4], vec![0; 4 * 128]),
            icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 4], vec![0; 4 * 128]),
        ));
        Self {
            dev,
            iface,
            sockets,
            dhcp: Some(dhcp),
            dns,
            icmp,
            tcp: [None; MAX_TCP],
            lease: None,
            dns_query: None,
            ping_ident: (seed as u16) | 1,
            next_port: 49152u16.saturating_add(((seed >> 16) as u16) % 8192),
            last_link: false,
        }
    }

    pub fn mac(&self) -> Mac {
        self.dev.nic.mac()
    }

    pub fn nic(&mut self) -> &mut N {
        &mut self.dev.nic
    }

    pub fn lease(&self) -> Option<&Lease> {
        self.lease.as_ref()
    }

    /// Drives the stack: receives frames, runs timers, transmits. Call at least every few ms.
    /// Returns `true` if the receive budget ran out (more frames may be waiting: poll again soon).
    pub fn poll(&mut self, now_ms: i64) -> bool {
        self.dev.budget = RX_BUDGET;
        self.dev.now_ms = now_ms;
        self.refresh_filter();
        let link = self.dev.nic.link_up();
        if link != self.last_link {
            self.last_link = link;
            if !link {
                self.deconfigure();
            } else if let Some(h) = self.dhcp {
                self.sockets.get_mut::<dhcpv4::Socket>(h).reset();
            }
        }
        if !link {
            return false;
        }
        self.iface
            .poll(instant(now_ms), &mut self.dev, &mut self.sockets);
        let event = self
            .dhcp
            .and_then(|h| self.sockets.get_mut::<dhcpv4::Socket>(h).poll());
        match event {
            None => {}
            Some(dhcpv4::Event::Deconfigured) => self.deconfigure(),
            Some(dhcpv4::Event::Configured(c)) => {
                let lease = Lease {
                    address: c.address,
                    router: c.router,
                    dns: c.dns_servers.iter().copied().collect(),
                };
                self.apply(lease);
            }
        }
        self.dev.budget == 0
    }

    /// Keeps the ingress filter in step with the address and the sockets that are listening.
    fn refresh_filter(&mut self) {
        let f = &mut self.dev.filter;
        f.addr = self.lease.as_ref().map(|l| l.address.address().octets());
        f.prefix = self.lease.as_ref().map_or(0, |l| l.address.prefix_len());
        f.addr6 = self
            .iface
            .ip_addrs()
            .iter()
            .filter_map(|c| match c {
                IpCidr::Ipv6(c) => Some(c.address().octets()),
                IpCidr::Ipv4(_) => None,
            })
            .collect();
        f.tcp_listen.clear();
        for h in self.tcp.iter().flatten() {
            let s = self.sockets.get::<tcp::Socket>(*h);
            if s.is_listening() {
                f.tcp_listen.push(s.listen_endpoint().port);
            }
        }
    }

    /// Answer ICMP echo requests from the local subnet (rate-limited). Off by default.
    pub fn set_echo_reply(&mut self, allow: bool) {
        self.dev.filter.allow_echo = allow;
    }

    /// Ingress-filter counters (passed / dropped frames, last drop reason).
    pub fn filter_stats(&self) -> crate::nic::Stats {
        self.dev.stats
    }

    fn deconfigure(&mut self) {
        self.lease = None;
        // Drop the IPv4 address only; IPv6 (link-local/SLAAC) is independent of DHCP.
        self.iface
            .update_ip_addrs(|a| a.retain(|c| matches!(c, IpCidr::Ipv6(_))));
        self.iface.routes_mut().remove_default_ipv4_route();
        // Established connections cannot survive losing the address.
        for slot in self.tcp.iter().flatten() {
            self.sockets.get_mut::<tcp::Socket>(*slot).abort();
        }
    }

    fn apply(&mut self, lease: Lease) {
        self.iface.update_ip_addrs(|a| {
            a.retain(|c| matches!(c, IpCidr::Ipv6(_)));
            let _ = a.push(IpCidr::Ipv4(lease.address));
        });
        match lease.router {
            Some(r) => {
                let _ = self.iface.routes_mut().add_default_ipv4_route(r);
            }
            None => {
                self.iface.routes_mut().remove_default_ipv4_route();
            }
        }
        let servers: Vec<IpAddress> = lease.dns.iter().map(|d| IpAddress::Ipv4(*d)).collect();
        self.sockets
            .get_mut::<dns::Socket>(self.dns)
            .update_servers(&servers);
        self.lease = Some(lease);
    }

    /// Configures a static address, bypassing DHCP (used for tests and recovery).
    pub fn set_static(&mut self, lease: Lease) {
        if let Some(h) = self.dhcp.take() {
            self.sockets.remove(h);
        }
        self.apply(lease);
    }

    // ---- ICMP ----

    /// Sends one echo request.
    pub fn ping(&mut self, dst: Ipv4Address, seq: u16) -> Result<(), Error> {
        if self.lease.is_none() {
            return Err(Error::NoLease);
        }
        let ident = self.ping_ident;
        let sock = self.sockets.get_mut::<icmp::Socket>(self.icmp);
        if !sock.is_open() {
            sock.bind(icmp::Endpoint::Ident(ident))
                .map_err(|_| Error::Invalid)?;
        }
        let data = b"KS_OS ping";
        let repr = Icmpv4Repr::EchoRequest {
            ident,
            seq_no: seq,
            data,
        };
        let buf = sock
            .send(repr.buffer_len(), IpAddress::Ipv4(dst))
            .map_err(|_| Error::Busy)?;
        repr.emit(
            &mut Icmpv4Packet::new_unchecked(buf),
            &smoltcp::phy::ChecksumCapabilities::default(),
        );
        Ok(())
    }

    /// Returns the next echo reply (IPv4 or IPv6), if one arrived.
    pub fn ping_result(&mut self) -> PingResult {
        let ident = self.ping_ident;
        let caps = smoltcp::phy::ChecksumCapabilities::ignored(); // the interface verified them on ingress
        let sock = self.sockets.get_mut::<icmp::Socket>(self.icmp);
        while let Ok((payload, from)) = sock.recv() {
            match from {
                IpAddress::Ipv4(from) => {
                    if let Ok(pkt) = Icmpv4Packet::new_checked(payload) {
                        if let Ok(Icmpv4Repr::EchoReply {
                            ident: i, seq_no, ..
                        }) = Icmpv4Repr::parse(&pkt, &caps)
                        {
                            if i == ident {
                                return PingResult::Reply { seq: seq_no, from };
                            }
                        }
                    }
                }
                IpAddress::Ipv6(from) => {
                    if let Ok(pkt) = Icmpv6Packet::new_checked(payload) {
                        if let Ok(Icmpv6Repr::EchoReply {
                            ident: i, seq_no, ..
                        }) = Icmpv6Repr::parse(&from, &from, &pkt, &caps)
                        {
                            if i == ident {
                                return PingResult::Reply6 { seq: seq_no, from };
                            }
                        }
                    }
                }
            }
        }
        PingResult::Pending
    }

    /// IPv6 addresses configured on the interface (link-local and SLAAC), if any.
    pub fn ipv6_addresses(&self) -> Vec<Ipv6Address> {
        self.iface
            .ip_addrs()
            .iter()
            .filter_map(|c| match c {
                IpCidr::Ipv6(c) => Some(c.address()),
                IpCidr::Ipv4(_) => None,
            })
            .collect()
    }

    /// A non-link-local IPv6 address (from SLAAC), if the network provides one.
    pub fn global_ipv6(&self) -> Option<Ipv6Address> {
        self.ipv6_addresses()
            .into_iter()
            .find(|a| !is_link_local(&a.octets()))
    }

    /// Sends one ICMPv6 echo request. Link-local destinations use the link-local source address.
    pub fn ping6(&mut self, dst: Ipv6Address, seq: u16) -> Result<(), Error> {
        let link_local = is_link_local(&dst.octets());
        let src = self
            .ipv6_addresses()
            .into_iter()
            .find(|a| is_link_local(&a.octets()) == link_local)
            .ok_or(Error::NoLease)?;
        let ident = self.ping_ident;
        let sock = self.sockets.get_mut::<icmp::Socket>(self.icmp);
        if !sock.is_open() {
            sock.bind(icmp::Endpoint::Ident(ident))
                .map_err(|_| Error::Invalid)?;
        }
        let repr = Icmpv6Repr::EchoRequest {
            ident,
            seq_no: seq,
            data: b"KS_OS ping",
        };
        let buf = sock
            .send(repr.buffer_len(), IpAddress::Ipv6(dst))
            .map_err(|_| Error::Busy)?;
        repr.emit(
            &src,
            &dst,
            &mut Icmpv6Packet::new_unchecked(buf),
            &smoltcp::phy::ChecksumCapabilities::default(),
        );
        Ok(())
    }

    // ---- DNS ----

    /// Starts resolving `name` to an IPv4 address (one query at a time).
    pub fn resolve(&mut self, name: &str) -> Result<(), Error> {
        if self.lease.is_none() {
            return Err(Error::NoLease);
        }
        if self.dns_query.is_some() {
            return Err(Error::Busy);
        }
        let h = self
            .sockets
            .get_mut::<dns::Socket>(self.dns)
            .start_query(self.iface.context(), name, DnsQueryType::A)
            .map_err(|_| Error::Invalid)?;
        self.dns_query = Some(h);
        Ok(())
    }

    /// `None` while pending, `Some(Err(()))` if resolution failed, `Some(Ok(addr))` on success.
    pub fn resolve_result(&mut self) -> Option<Result<Ipv4Address, ()>> {
        let h = self.dns_query?;
        match self
            .sockets
            .get_mut::<dns::Socket>(self.dns)
            .get_query_result(h)
        {
            Err(dns::GetQueryResultError::Pending) => None,
            Err(dns::GetQueryResultError::Failed) => {
                self.dns_query = None;
                Some(Err(()))
            }
            Ok(addrs) => {
                self.dns_query = None;
                // Only IPv4 results are used for now (the TCP API is IPv4).
                let a = addrs.iter().find_map(|a| match a {
                    IpAddress::Ipv4(a) => Some(*a),
                    IpAddress::Ipv6(_) => None,
                });
                Some(a.ok_or(()))
            }
        }
    }

    // ---- TCP ----

    pub fn tcp_connect(&mut self, addr: Ipv4Address, port: u16) -> Result<TcpId, Error> {
        if self.lease.is_none() {
            return Err(Error::NoLease);
        }
        let slot = self
            .tcp
            .iter()
            .position(Option::is_none)
            .ok_or(Error::NoSlot)?;
        let mut s = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; TCP_BUF]),
            tcp::SocketBuffer::new(vec![0; TCP_BUF]),
        );
        let local = self.next_port;
        self.next_port = if self.next_port >= 65000 {
            49152
        } else {
            self.next_port.saturating_add(1)
        };
        s.connect(
            self.iface.context(),
            IpEndpoint::new(IpAddress::Ipv4(addr), port),
            local,
        )
        .map_err(|_| Error::Invalid)?;
        s.set_nagle_enabled(false);
        *self.tcp.get_mut(slot).ok_or(Error::NoSlot)? = Some(self.sockets.add(s));
        Ok(TcpId(slot))
    }

    /// Listens on `port`; the returned socket becomes `Established` when a peer connects
    /// (single-shot: call again for the next connection).
    pub fn tcp_listen(&mut self, port: u16) -> Result<TcpId, Error> {
        let slot = self
            .tcp
            .iter()
            .position(Option::is_none)
            .ok_or(Error::NoSlot)?;
        let mut s = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; TCP_BUF]),
            tcp::SocketBuffer::new(vec![0; TCP_BUF]),
        );
        s.listen(port).map_err(|_| Error::Invalid)?;
        *self.tcp.get_mut(slot).ok_or(Error::NoSlot)? = Some(self.sockets.add(s));
        Ok(TcpId(slot))
    }

    fn tcp_sock(&mut self, id: TcpId) -> Option<&mut tcp::Socket<'static>> {
        let h = (*self.tcp.get(id.0)?)?;
        Some(self.sockets.get_mut::<tcp::Socket>(h))
    }

    pub fn tcp_state(&mut self, id: TcpId) -> TcpState {
        let Some(s) = self.tcp_sock(id) else {
            return TcpState::Closed;
        };
        match s.state() {
            tcp::State::Listen => TcpState::Listening,
            tcp::State::SynSent | tcp::State::SynReceived => TcpState::Connecting,
            tcp::State::Established => TcpState::Established,
            tcp::State::CloseWait => TcpState::PeerClosed,
            tcp::State::FinWait1
            | tcp::State::FinWait2
            | tcp::State::Closing
            | tcp::State::LastAck => TcpState::PeerClosed,
            _ => TcpState::Closed,
        }
    }

    /// Queues data for sending; returns how many bytes fit.
    pub fn tcp_send(&mut self, id: TcpId, data: &[u8]) -> usize {
        self.tcp_sock(id)
            .and_then(|s| s.send_slice(data).ok())
            .unwrap_or(0)
    }

    /// Reads received data; returns the byte count (0 if none is available).
    pub fn tcp_recv(&mut self, id: TcpId, buf: &mut [u8]) -> usize {
        self.tcp_sock(id)
            .and_then(|s| s.recv_slice(buf).ok())
            .unwrap_or(0)
    }

    /// Starts a graceful close.
    pub fn tcp_close(&mut self, id: TcpId) {
        if let Some(s) = self.tcp_sock(id) {
            s.close();
        }
    }

    /// Frees the socket slot (aborting the connection if it is still open).
    pub fn tcp_release(&mut self, id: TcpId) {
        if let Some(h) = self.tcp.get_mut(id.0).and_then(Option::take) {
            self.sockets.get_mut::<tcp::Socket>(h).abort();
            self.sockets.remove(h);
        }
    }
}
