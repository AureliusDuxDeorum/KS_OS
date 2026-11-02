// SPDX-License-Identifier: Apache-2.0
//! Simulation helpers: DMA memory model, an Ethernet "wire", and a scripted DHCP/DNS server.
#![allow(dead_code)]

use ksnet::dma::DmaMem;
use ksnet::Nic;
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

/// Byte-addressable memory with interior mutability, like device-shared RAM.
pub struct Mem(pub RefCell<Vec<u8>>);

impl Mem {
    pub fn new(len: usize) -> Self {
        Self(RefCell::new(vec![0; len]))
    }
}

impl DmaMem for Mem {
    fn len(&self) -> usize {
        self.0.borrow().len()
    }
    fn read32(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.0.borrow()[off..off + 4].try_into().unwrap())
    }
    fn write32(&self, off: usize, v: u32) {
        self.0.borrow_mut()[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn read_bytes(&self, off: usize, out: &mut [u8]) {
        out.copy_from_slice(&self.0.borrow()[off..off + out.len()]);
    }
    fn write_bytes(&self, off: usize, data: &[u8]) {
        self.0.borrow_mut()[off..off + data.len()].copy_from_slice(data);
    }
}

pub type Queue = Rc<RefCell<VecDeque<Vec<u8>>>>;

/// One end of a point-to-point wire.
pub struct WireNic {
    pub mac: [u8; 6],
    pub rx: Queue,
    pub tx: Queue,
    pub up: Rc<Cell<bool>>,
    /// Drop every n-th transmitted frame (0 = never) to exercise retransmission.
    pub drop_every: u32,
    sent: u32,
}

impl Nic for WireNic {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn link_up(&mut self) -> bool {
        self.up.get()
    }
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        let f = self.rx.borrow_mut().pop_front()?;
        buf[..f.len()].copy_from_slice(&f);
        Some(f.len())
    }
    fn transmit(&mut self, frame: &[u8]) -> bool {
        self.sent += 1;
        if self.drop_every != 0 && self.sent.is_multiple_of(self.drop_every) {
            return true;
        }
        self.tx.borrow_mut().push_back(frame.to_vec());
        true
    }
}

pub fn wire_pair(mac_a: [u8; 6], mac_b: [u8; 6]) -> (WireNic, WireNic, Rc<Cell<bool>>) {
    let a2b: Queue = Default::default();
    let b2a: Queue = Default::default();
    let up = Rc::new(Cell::new(true));
    (
        WireNic {
            mac: mac_a,
            rx: b2a.clone(),
            tx: a2b.clone(),
            up: up.clone(),
            drop_every: 0,
            sent: 0,
        },
        WireNic {
            mac: mac_b,
            rx: a2b,
            tx: b2a,
            up: up.clone(),
            drop_every: 0,
            sent: 0,
        },
        up,
    )
}

pub const SERVER_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
pub const CLIENT_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);
pub const DNS_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 3);
pub const NAME_IP: Ipv4Address = Ipv4Address::new(93, 184, 216, 34);
pub const SERVER_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];

/// A server-side NIC that answers DHCP and DNS (and ARP for the DNS address) by hand and passes
/// everything else to the real server stack on top of it.
pub struct ServerNic {
    pub inner: WireNic,
    pub pending: VecDeque<Vec<u8>>,
    pub dhcp_msgs: Vec<DhcpMessageType>,
    pub dns_queries: Vec<String>,
    pub answer_dns: bool,
}

impl ServerNic {
    pub fn new(inner: WireNic) -> Self {
        Self {
            inner,
            pending: VecDeque::new(),
            dhcp_msgs: vec![],
            dns_queries: vec![],
            answer_dns: true,
        }
    }

    fn udp_reply(
        &self,
        to_mac: EthernetAddress,
        src: Ipv4Address,
        dst: Ipv4Address,
        sport: u16,
        dport: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let caps = ChecksumCapabilities::default();
        let udp = UdpRepr {
            src_port: sport,
            dst_port: dport,
        };
        let ip = Ipv4Repr {
            src_addr: src,
            dst_addr: dst,
            next_header: IpProtocol::Udp,
            payload_len: 8 + payload.len(),
            hop_limit: 64,
        };
        let eth = EthernetRepr {
            src_addr: EthernetAddress(SERVER_MAC),
            dst_addr: to_mac,
            ethertype: EthernetProtocol::Ipv4,
        };
        let mut buf = vec![0u8; 14 + 20 + 8 + payload.len()];
        eth.emit(&mut EthernetFrame::new_unchecked(&mut buf[..]));
        ip.emit(&mut Ipv4Packet::new_unchecked(&mut buf[14..]), &caps);
        udp.emit(
            &mut UdpPacket::new_unchecked(&mut buf[34..]),
            &IpAddress::Ipv4(src),
            &IpAddress::Ipv4(dst),
            payload.len(),
            |p| p.copy_from_slice(payload),
            &caps,
        );
        buf
    }

    /// Returns true if the frame was consumed.
    fn intercept(&mut self, frame: &[u8]) -> bool {
        let Ok(eth) = EthernetFrame::new_checked(frame) else {
            return false;
        };
        // ARP for the DNS server address (the stack under test may need it).
        if eth.ethertype() == EthernetProtocol::Arp {
            if let Ok(p) = ArpPacket::new_checked(eth.payload()) {
                if let Ok(ArpRepr::EthernetIpv4 {
                    operation: ArpOperation::Request,
                    source_hardware_addr,
                    source_protocol_addr,
                    target_protocol_addr,
                    ..
                }) = ArpRepr::parse(&p)
                {
                    if target_protocol_addr == DNS_IP {
                        let r = ArpRepr::EthernetIpv4 {
                            operation: ArpOperation::Reply,
                            source_hardware_addr: EthernetAddress(SERVER_MAC),
                            source_protocol_addr: DNS_IP,
                            target_hardware_addr: source_hardware_addr,
                            target_protocol_addr: source_protocol_addr,
                        };
                        let mut buf = vec![0u8; 14 + r.buffer_len()];
                        EthernetRepr {
                            src_addr: EthernetAddress(SERVER_MAC),
                            dst_addr: source_hardware_addr,
                            ethertype: EthernetProtocol::Arp,
                        }
                        .emit(&mut EthernetFrame::new_unchecked(&mut buf[..]));
                        r.emit(&mut ArpPacket::new_unchecked(&mut buf[14..]));
                        self.pending.push_back(buf);
                        return true;
                    }
                }
            }
            return false;
        }
        if eth.ethertype() != EthernetProtocol::Ipv4 {
            return false;
        }
        let Ok(ip) = Ipv4Packet::new_checked(eth.payload()) else {
            return false;
        };
        if ip.next_header() != IpProtocol::Udp {
            return false;
        }
        let Ok(udp) = UdpPacket::new_checked(ip.payload()) else {
            return false;
        };
        let client_mac = eth.src_addr();
        if udp.dst_port() == 67 {
            let Ok(pkt) = DhcpPacket::new_checked(udp.payload()) else {
                return true;
            };
            let Ok(req) = DhcpRepr::parse(&pkt) else {
                return true;
            };
            self.dhcp_msgs.push(req.message_type);
            let reply_type = match req.message_type {
                DhcpMessageType::Discover => DhcpMessageType::Offer,
                DhcpMessageType::Request => DhcpMessageType::Ack,
                _ => return true,
            };
            let mut dns = heapless::Vec::<Ipv4Address, 3>::new();
            dns.push(DNS_IP).unwrap();
            let rep = DhcpRepr {
                message_type: reply_type,
                transaction_id: req.transaction_id,
                secs: 0,
                client_hardware_address: req.client_hardware_address,
                client_ip: Ipv4Address::UNSPECIFIED,
                your_ip: CLIENT_IP,
                server_ip: SERVER_IP,
                router: Some(SERVER_IP),
                subnet_mask: Some(Ipv4Address::new(255, 255, 255, 0)),
                relay_agent_ip: Ipv4Address::UNSPECIFIED,
                broadcast: false,
                requested_ip: None,
                client_identifier: None,
                server_identifier: Some(SERVER_IP),
                parameter_request_list: None,
                dns_servers: Some(dns),
                max_size: None,
                lease_duration: Some(3600),
                renew_duration: None,
                rebind_duration: None,
                additional_options: &[],
            };
            let mut body = vec![0u8; rep.buffer_len()];
            rep.emit(&mut DhcpPacket::new_unchecked(&mut body[..]))
                .unwrap();
            self.pending.push_back(self.udp_reply(
                client_mac,
                SERVER_IP,
                Ipv4Address::BROADCAST,
                67,
                68,
                &body,
            ));
            return true;
        }
        if udp.dst_port() == 53 && ip.dst_addr() == DNS_IP {
            let q = udp.payload();
            if q.len() < 12 {
                return true;
            }
            // Decode the first question name for the log.
            let mut name = String::new();
            let mut i = 12;
            while i < q.len() && q[i] != 0 {
                let l = q[i] as usize;
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(std::str::from_utf8(&q[i + 1..i + 1 + l]).unwrap());
                i += l + 1;
            }
            self.dns_queries.push(name);
            if !self.answer_dns {
                return true;
            }
            let qend = i + 1 + 4;
            let mut r = q[..qend].to_vec();
            r[2] = 0x81;
            r[3] = 0x80; // response, recursion available, no error
            r[6] = 0;
            r[7] = 1; // ANCOUNT = 1
            r.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
            r.extend_from_slice(&NAME_IP.octets());
            self.pending.push_back(self.udp_reply(
                client_mac,
                DNS_IP,
                ip.src_addr(),
                53,
                udp.src_port(),
                &r,
            ));
            return true;
        }
        false
    }
}

impl Nic for ServerNic {
    fn mac(&self) -> [u8; 6] {
        self.inner.mac
    }
    fn link_up(&mut self) -> bool {
        self.inner.link_up()
    }
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        // Hand-made replies first (they go back to the client via `transmit` semantics below).
        loop {
            let mut tmp = [0u8; 1600];
            let n = self.inner.receive(&mut tmp)?;
            if self.intercept(&tmp[..n]) {
                while let Some(f) = self.pending.pop_front() {
                    self.inner.transmit(&f);
                }
                continue;
            }
            buf[..n].copy_from_slice(&tmp[..n]);
            return Some(n);
        }
    }
    fn transmit(&mut self, frame: &[u8]) -> bool {
        self.inner.transmit(frame)
    }
}
