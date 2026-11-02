// SPDX-License-Identifier: Apache-2.0
//! The Bluetooth host: drives a controller through a [`Transport`] and exposes connected peers
//! as ATT and SMP channels.
//!
//! Everything is synchronous: calls pump the transport until the awaited response arrives or a
//! timeout (measured by the transport's clock) expires. Server-initiated ATT notifications are
//! queued for [`Host::next_notification`].

use crate::gatt::{self, AttLink};
use crate::hci::{self, AdvReport, Command, Event, STATUS_SUCCESS};
use crate::l2cap::{self, Reassembler};
use crate::smp::SmpLink;
use crate::Address;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// A packet from the controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// Event packet (code, length, parameters).
    Event(Vec<u8>),
    /// ACL data packet (header + payload).
    Acl(Vec<u8>),
}

/// The byte-level link to a controller (USB, UART, or a simulator).
pub trait Transport {
    fn send_command(&mut self, packet: &[u8]) -> Result<(), HostError>;
    fn send_acl(&mut self, packet: &[u8]) -> Result<(), HostError>;
    /// Waits up to `timeout_ms` for the next packet.
    fn recv(&mut self, timeout_ms: u64) -> Option<Packet>;
    /// Monotonic milliseconds.
    fn now_ms(&mut self) -> u64;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostError {
    Transport,
    Timeout,
    /// The controller rejected a command with this HCI status.
    Status(u8),
    Disconnected,
    Malformed,
    Unsupported(&'static str),
}

impl From<HostError> for gatt::Error {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Timeout => gatt::Error::Timeout,
            HostError::Disconnected => gatt::Error::Disconnected,
            _ => gatt::Error::Malformed,
        }
    }
}

/// What the controller told us during initialisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerInfo {
    pub version: hci::LocalVersion,
    pub address: Address,
    pub acl_len: u16,
    pub acl_buffers: u8,
}

struct Connection {
    handle: u16,
    peer: Address,
    reasm: Reassembler,
    encrypted: bool,
    att: VecDeque<Vec<u8>>,
    smp: VecDeque<Vec<u8>>,
}

pub struct Host<T: Transport> {
    pub transport: T,
    events: VecDeque<Event>,
    conns: Vec<Connection>,
    notifications: VecDeque<(u16, u16, Vec<u8>)>,
    acl_len: usize,
    credits: u32,
    pub info: Option<ControllerInfo>,
}

const COMMAND_TIMEOUT_MS: u64 = 2000;

impl<T: Transport> Host<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            events: VecDeque::new(),
            conns: Vec::new(),
            notifications: VecDeque::new(),
            acl_len: 27,
            credits: 0,
            info: None,
        }
    }

    // ---- packet pump -------------------------------------------------------------------

    /// Receives and processes one packet; returns false if nothing arrived.
    pub fn pump(&mut self, timeout_ms: u64) -> bool {
        match self.transport.recv(timeout_ms) {
            Some(Packet::Event(p)) => {
                if let Some(ev) = hci::parse_event(&p) {
                    self.on_event(ev);
                }
                true
            }
            Some(Packet::Acl(p)) => {
                self.on_acl(&p);
                true
            }
            None => false,
        }
    }

    fn on_event(&mut self, ev: Event) {
        match &ev {
            Event::Disconnection { handle, .. } => self.conns.retain(|c| c.handle != *handle),
            Event::NumCompletedPackets(list) => {
                for (_, n) in list {
                    self.credits += u32::from(*n);
                }
            }
            Event::EncryptionChange {
                status: STATUS_SUCCESS,
                handle,
                enabled,
            } => {
                if let Some(c) = self.conns.iter_mut().find(|c| c.handle == *handle) {
                    c.encrypted = *enabled;
                }
            }
            Event::LeConnectionComplete {
                status: STATUS_SUCCESS,
                handle,
                peer,
                ..
            } if !self.conns.iter().any(|c| c.handle == *handle) => {
                self.conns.push(Connection {
                    handle: *handle,
                    peer: *peer,
                    reasm: Reassembler::default(),
                    encrypted: false,
                    att: VecDeque::new(),
                    smp: VecDeque::new(),
                });
            }
            _ => {}
        }
        if self.events.len() < 256 {
            self.events.push_back(ev);
        }
    }

    fn on_acl(&mut self, pkt: &[u8]) {
        let Some((h, data)) = hci::parse_acl(pkt) else {
            return;
        };
        let Some(conn) = self.conns.iter_mut().find(|c| c.handle == h.handle) else {
            return;
        };
        let Some(frame) = conn.reasm.push(&h, data) else {
            return;
        };
        match frame.cid {
            l2cap::CID_ATT => match frame.payload.first() {
                Some(&op) if crate::att::is_server_initiated(op) && frame.payload.len() >= 3 => {
                    let handle = u16::from_le_bytes([frame.payload[1], frame.payload[2]]);
                    if self.notifications.len() < 64 {
                        self.notifications.push_back((
                            h.handle,
                            handle,
                            frame.payload[3..].to_vec(),
                        ));
                    }
                    if op == crate::att::OP_HANDLE_VALUE_IND {
                        let _ = self.send_l2cap(
                            h.handle,
                            l2cap::CID_ATT,
                            &[crate::att::OP_HANDLE_VALUE_CONF],
                        );
                    }
                }
                Some(_) if conn.att.len() < 16 => conn.att.push_back(frame.payload),
                _ => {}
            },
            l2cap::CID_SMP if conn.smp.len() < 16 => conn.smp.push_back(frame.payload),
            l2cap::CID_LE_SIGNALING => {
                if let Some(u) = l2cap::parse_signaling(&frame.payload) {
                    let ok = l2cap::valid_connection_parameters(&u);
                    let _ = self.send_l2cap(
                        h.handle,
                        l2cap::CID_LE_SIGNALING,
                        &l2cap::signaling_update_response(u.identifier, ok),
                    );
                    if ok {
                        let _ = self.send_command(&Command::le_connection_update(
                            h.handle,
                            u.interval_max,
                            u.latency,
                            u.timeout,
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    fn send_command(&mut self, cmd: &Command) -> Result<(), HostError> {
        self.transport.send_command(&cmd.to_bytes())
    }

    fn send_l2cap(&mut self, handle: u16, cid: u16, payload: &[u8]) -> Result<(), HostError> {
        for pkt in l2cap::fragment(handle, cid, payload, self.acl_len) {
            // Respect the controller's buffer count; wait for completions when exhausted.
            let deadline = self.transport.now_ms() + 1000;
            while self.credits == 0 && self.info.is_some() {
                if self.transport.now_ms() > deadline {
                    return Err(HostError::Timeout);
                }
                self.pump(20);
            }
            self.credits = self.credits.saturating_sub(1);
            self.transport.send_acl(&pkt)?;
        }
        Ok(())
    }

    // ---- commands ----------------------------------------------------------------------

    /// Sends a command and waits for its completion; returns the return parameters after the status.
    pub fn command(&mut self, cmd: Command) -> Result<Vec<u8>, HostError> {
        self.send_command(&cmd)?;
        let deadline = self.transport.now_ms() + COMMAND_TIMEOUT_MS;
        loop {
            // Look through everything already received first.
            if let Some(pos) = self.events.iter().position(|e| matches!(e, Event::CommandComplete { opcode, .. } | Event::CommandStatus { opcode, .. } if *opcode == cmd.opcode)) {
                return match self.events.remove(pos) {
                    Some(Event::CommandComplete { status: STATUS_SUCCESS, ret, .. }) => Ok(ret),
                    Some(Event::CommandComplete { status, .. } | Event::CommandStatus { status, .. }) if status != STATUS_SUCCESS => Err(HostError::Status(status)),
                    Some(_) => Ok(Vec::new()),
                    None => Err(HostError::Malformed),
                };
            }
            if self.transport.now_ms() > deadline {
                return Err(HostError::Timeout);
            }
            self.pump(50);
        }
    }

    /// Resets and configures the controller for LE central operation.
    pub fn init(&mut self) -> Result<ControllerInfo, HostError> {
        self.command(Command::reset())?;
        let version = hci::parse_local_version(&self.command(Command::read_local_version())?)
            .ok_or(HostError::Malformed)?;
        let addr = self.command(Command::read_bd_addr())?;
        let bytes: [u8; 6] = addr
            .get(..6)
            .ok_or(HostError::Malformed)?
            .try_into()
            .map_err(|_| HostError::Malformed)?;
        let (acl_len, acl_buffers) =
            hci::parse_le_buffer_size(&self.command(Command::le_read_buffer_size())?)
                .ok_or(HostError::Malformed)?;
        if acl_len == 0 || acl_buffers == 0 {
            return Err(HostError::Unsupported(
                "controller shares its ACL buffers with BR/EDR (not supported yet)",
            ));
        }
        // Events we act on: disconnection, encryption change, command complete/status, number of
        // completed packets, LE meta (bits per Core Spec Vol 4 Part E 7.3.1).
        self.command(Command::set_event_mask(0x2000_1FFF_FFFF_FFFF))?;
        self.command(Command::write_le_host_supported(true)).ok(); // best effort: some controllers refuse
                                                                   // LE events: connection complete, advertising report, connection update, LTK request, enhanced connection complete.
        self.command(Command::le_set_event_mask(0x0000_0000_0000_061F))?;
        self.acl_len = usize::from(acl_len).max(27);
        self.credits = u32::from(acl_buffers);
        let info = ControllerInfo {
            version,
            address: Address::new(0, bytes),
            acl_len,
            acl_buffers,
        };
        self.info = Some(info);
        Ok(info)
    }

    // ---- scanning and connecting ---------------------------------------------------------

    /// Scans for `duration_ms`, calling `on_report` for each advertising report.
    pub fn scan(
        &mut self,
        duration_ms: u64,
        mut on_report: impl FnMut(&AdvReport),
    ) -> Result<(), HostError> {
        self.command(Command::le_set_scan_parameters(true, 0x0060, 0x0030))?;
        self.command(Command::le_set_scan_enable(true, false))?;
        let deadline = self.transport.now_ms() + duration_ms;
        while self.transport.now_ms() < deadline {
            self.pump(20);
            while let Some(i) = self
                .events
                .iter()
                .position(|e| matches!(e, Event::LeAdvertisingReports(_)))
            {
                if let Some(Event::LeAdvertisingReports(list)) = self.events.remove(i) {
                    list.iter().for_each(&mut on_report);
                }
            }
        }
        self.command(Command::le_set_scan_enable(false, false))?;
        Ok(())
    }

    /// Connects to `peer` and returns the connection handle.
    pub fn connect(&mut self, peer: Address, timeout_ms: u64) -> Result<u16, HostError> {
        // 7.5 ms .. 30 ms are typical HID intervals; request 15 ms (units of 1.25 ms), 6 s supervision.
        self.command(Command::le_create_connection(peer, 12, 0, 600))?;
        let deadline = self.transport.now_ms() + timeout_ms;
        loop {
            if let Some(i) = self
                .events
                .iter()
                .position(|e| matches!(e, Event::LeConnectionComplete { .. }))
            {
                if let Some(Event::LeConnectionComplete { status, handle, .. }) =
                    self.events.remove(i)
                {
                    return if status == STATUS_SUCCESS {
                        Ok(handle)
                    } else {
                        Err(HostError::Status(status))
                    };
                }
            }
            if self.transport.now_ms() > deadline {
                let _ = self.command(Command::le_create_connection_cancel());
                return Err(HostError::Timeout);
            }
            self.pump(50);
        }
    }

    pub fn disconnect(&mut self, handle: u16) {
        let _ = self.command(Command::disconnect(handle, 0x13));
    }

    pub fn is_connected(&self, handle: u16) -> bool {
        self.conns.iter().any(|c| c.handle == handle)
    }

    pub fn is_encrypted(&self, handle: u16) -> bool {
        self.conns.iter().any(|c| c.handle == handle && c.encrypted)
    }

    /// Starts link-layer encryption with `ltk` (LE Secure Connections: `rand = 0`, `ediv = 0`)
    /// and waits for the result.
    pub fn start_encryption(&mut self, handle: u16, ltk: [u8; 16]) -> Result<(), HostError> {
        self.command(Command::le_start_encryption(handle, [0; 8], 0, ltk))?;
        let deadline = self.transport.now_ms() + 5000;
        loop {
            if let Some(i) = self.events.iter().position(
                |e| matches!(e, Event::EncryptionChange { handle: h, .. } if *h == handle),
            ) {
                if let Some(Event::EncryptionChange {
                    status, enabled, ..
                }) = self.events.remove(i)
                {
                    return if status == STATUS_SUCCESS && enabled {
                        Ok(())
                    } else {
                        Err(HostError::Status(status))
                    };
                }
            }
            if !self.is_connected(handle) {
                return Err(HostError::Disconnected);
            }
            if self.transport.now_ms() > deadline {
                return Err(HostError::Timeout);
            }
            self.pump(50);
        }
    }

    // ---- channels ----------------------------------------------------------------------

    pub fn att(&mut self, handle: u16) -> AttChannel<'_, T> {
        AttChannel { host: self, handle }
    }

    pub fn smp(&mut self, handle: u16) -> SmpChannel<'_, T> {
        SmpChannel { host: self, handle }
    }

    /// Next queued ATT notification `(connection handle, attribute handle, value)`.
    pub fn next_notification(&mut self) -> Option<(u16, u16, Vec<u8>)> {
        self.notifications.pop_front()
    }

    /// Waits up to `timeout_ms` for a notification.
    pub fn wait_notification(&mut self, timeout_ms: u64) -> Option<(u16, u16, Vec<u8>)> {
        if let Some(n) = self.next_notification() {
            return Some(n);
        }
        self.pump(timeout_ms);
        self.next_notification()
    }

    pub fn peer(&self, handle: u16) -> Option<Address> {
        self.conns
            .iter()
            .find(|c| c.handle == handle)
            .map(|c| c.peer)
    }
}

pub struct AttChannel<'a, T: Transport> {
    host: &'a mut Host<T>,
    handle: u16,
}

impl<T: Transport> AttLink for AttChannel<'_, T> {
    fn request(&mut self, pdu: &[u8]) -> Result<Vec<u8>, gatt::Error> {
        self.host.send_l2cap(self.handle, l2cap::CID_ATT, pdu)?;
        let deadline = self.host.transport.now_ms() + 5000;
        loop {
            let conn = self
                .host
                .conns
                .iter_mut()
                .find(|c| c.handle == self.handle)
                .ok_or(gatt::Error::Disconnected)?;
            if let Some(rsp) = conn.att.pop_front() {
                return Ok(rsp);
            }
            if self.host.transport.now_ms() > deadline {
                return Err(gatt::Error::Timeout);
            }
            self.host.pump(50);
        }
    }
}

pub struct SmpChannel<'a, T: Transport> {
    host: &'a mut Host<T>,
    handle: u16,
}

impl<T: Transport> SmpLink for SmpChannel<'_, T> {
    fn send(&mut self, pdu: &[u8]) -> Result<(), gatt::Error> {
        self.host
            .send_l2cap(self.handle, l2cap::CID_SMP, pdu)
            .map_err(Into::into)
    }

    fn recv(&mut self, timeout_ms: u64) -> Result<Vec<u8>, gatt::Error> {
        let deadline = self.host.transport.now_ms() + timeout_ms;
        loop {
            let conn = self
                .host
                .conns
                .iter_mut()
                .find(|c| c.handle == self.handle)
                .ok_or(gatt::Error::Disconnected)?;
            if let Some(p) = conn.smp.pop_front() {
                return Ok(p);
            }
            if self.host.transport.now_ms() > deadline {
                return Err(gatt::Error::Timeout);
            }
            self.host.pump(50);
        }
    }
}
