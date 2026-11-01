//! A simulated Bluetooth LE controller and HID keyboard peripheral, used to test the whole host
//! stack. The peripheral side (GATT server, SMP responder, link-layer encryption check) is written
//! independently of the host code paths it tests, sharing only the verified crypto functions.

#![allow(dead_code)]

use ksbt::att::*;
use ksbt::crypto::{self, EphemeralKey};
use ksbt::hci::{self, op};
use ksbt::host::{HostError, Packet, Transport};
use ksbt::l2cap::{self, Reassembler};
use ksbt::smp as smpx;
use ksbt::{Address, Rng};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

pub struct TestRng(pub u64);
impl Rng for TestRng {
    fn fill(&mut self, buf: &mut [u8]) {
        for b in buf {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            *b = (self.0 >> 24) as u8;
        }
    }
}

pub const HOST_ADDR: Address = Address::new(0, [0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
pub const PERIPH_ADDR: Address = Address::new(0, [0xCE, 0xBF, 0x37, 0x37, 0x12, 0x56]);

/// A combined keyboard (report 1) and mouse (report 2) HID report map using report IDs.
pub fn report_map() -> Vec<u8> {
    let mut m = vec![0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01];
    m.extend_from_slice(&[
        0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81,
        0x02,
    ]);
    m.extend_from_slice(&[0x95, 0x01, 0x75, 0x08, 0x81, 0x01]);
    m.extend_from_slice(&[
        0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81,
        0x00, 0xC0,
    ]);
    m.extend_from_slice(&[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19,
        0x01, 0x29, 0x03,
    ]);
    m.extend_from_slice(&[
        0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81,
        0x03,
    ]);
    m.extend_from_slice(&[
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81,
        0x06, 0xC0, 0xC0,
    ]);
    m
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Behaviour {
    Normal,
    /// Sends an off-curve public key.
    BadPublicKey,
    /// Replies with a Pairing Response lacking Secure Connections.
    LegacyOnly,
    /// Offers a 7-byte encryption key.
    WeakKey,
    /// Reflects the initiator's public key back.
    ReflectKey,
    /// Replies to every ATT request with the same malformed "more data" response forever.
    HostileAtt,
}

/// Shared handles so tests can observe/feed user interaction.
#[derive(Default)]
pub struct Shared {
    /// The passkey the *host* displayed (what a human would type on the keyboard).
    pub displayed_passkey: Cell<Option<u32>>,
    /// The passkey the simulated keyboard "types"; defaults to the displayed one.
    pub typed_passkey: Cell<Option<u32>>,
    pub peripheral_numeric: Cell<Option<u32>>,
    pub param_update_response: Cell<Option<bool>>,
    pub update_command_seen: Cell<bool>,
    pub enc_start_attempts: Cell<u32>,
}

struct SmpState {
    req: Vec<u8>,
    rsp: Vec<u8>,
    key: Option<EphemeralKey>,
    pk_a: ([u8; 32], [u8; 32]),
    pk_b: ([u8; 32], [u8; 32]),
    dh: [u8; 32],
    nb: [u8; 16],
    na: [u8; 16],
    ca_round: [u8; 16],
    round: u32,
    passkey: u32,
    method: smpx::Method,
    ltk: Option<[u8; 16]>,
    mac_key: [u8; 16],
}

pub struct Sim {
    pub clock: u64,
    queue: VecDeque<Packet>,
    pub shared: Rc<Shared>,
    pub behaviour: Behaviour,
    pub periph_io: u8,
    pub discoverable: bool,
    scanning: bool,
    connected: Option<u16>,
    next_handle: u16,
    reasm: Reassembler,
    pub encrypted: bool,
    pub ccc_enabled: Cell<bool>,
    smp: Option<SmpState>,
    pub periph_ltk: Option<[u8; 16]>,
    rng: TestRng,
    pending_connect: Option<u64>,
    pub log: RefCell<Vec<String>>,
    adv_sent: u32,
    pub acl_len: u16,
    pub unsolicited_garbage: bool,
    pub mtu: usize,
}

impl Sim {
    pub fn new(shared: Rc<Shared>) -> Self {
        Self {
            clock: 0,
            queue: VecDeque::new(),
            shared,
            behaviour: Behaviour::Normal,
            periph_io: smpx::io::NO_INPUT_NO_OUTPUT,
            discoverable: true,
            scanning: false,
            connected: None,
            next_handle: 0x40,
            reasm: Reassembler::default(),
            encrypted: false,
            ccc_enabled: Cell::new(false),
            smp: None,
            periph_ltk: None,
            rng: TestRng(0x00C0_FFEE_1234_5678),
            pending_connect: None,
            log: RefCell::new(Vec::new()),
            adv_sent: 0,
            acl_len: 27,
            unsolicited_garbage: false,
            mtu: 23,
        }
    }

    fn event(&mut self, code: u8, params: &[u8]) {
        let mut p = vec![code, params.len() as u8];
        p.extend_from_slice(params);
        self.queue.push_back(Packet::Event(p));
    }

    fn command_complete(&mut self, opcode: u16, ret: &[u8]) {
        let mut p = vec![1u8];
        p.extend_from_slice(&opcode.to_le_bytes());
        p.push(0); // status
        p.extend_from_slice(ret);
        self.event(hci::evt::COMMAND_COMPLETE, &p);
    }

    fn command_status(&mut self, opcode: u16, status: u8) {
        let mut p = vec![status, 1];
        p.extend_from_slice(&opcode.to_le_bytes());
        self.event(hci::evt::COMMAND_STATUS, &p);
    }

    fn send_l2cap(&mut self, cid: u16, payload: &[u8]) {
        let handle = self.connected.expect("connected");
        for pkt in l2cap::fragment(handle, cid, payload, usize::from(self.acl_len)) {
            self.queue.push_back(Packet::Acl(pkt));
        }
    }

    /// Delivers a key press (usage `key`, modifiers `mods`) as a HID report-1 notification.
    pub fn press_key(&mut self, mods: u8, key: u8) {
        let mut v = vec![OP_HANDLE_VALUE_NTF, 0x1A, 0x00];
        v.extend_from_slice(&[mods, 0, key, 0, 0, 0, 0, 0]);
        self.send_l2cap(l2cap::CID_ATT, &v);
    }

    /// Delivers mouse motion as a report-2 notification.
    pub fn move_mouse(&mut self, buttons: u8, dx: i8, dy: i8) {
        let v = vec![OP_HANDLE_VALUE_NTF, 0x1E, 0x00, buttons, dx as u8, dy as u8];
        self.send_l2cap(l2cap::CID_ATT, &v);
    }

    pub fn request_parameter_update(&mut self) {
        let mut p = vec![l2cap::sig::CONNECTION_PARAMETER_UPDATE_REQUEST, 7, 8, 0];
        for v in [12u16, 24, 0, 300] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        self.send_l2cap(l2cap::CID_LE_SIGNALING, &p);
    }

    // ---- HCI commands --------------------------------------------------------------------

    fn handle_command(&mut self, c: &[u8]) {
        let opcode = u16::from_le_bytes([c[0], c[1]]);
        let params = &c[3..];
        match opcode {
            op::RESET => self.command_complete(opcode, &[]),
            op::READ_LOCAL_VERSION => {
                self.command_complete(opcode, &[0x0B, 0x34, 0x12, 0x0B, 0x8F, 0x00, 0x56, 0x78])
            }
            op::READ_BD_ADDR => self.command_complete(opcode, &HOST_ADDR.bytes),
            op::LE_READ_BUFFER_SIZE => {
                let mut r = self.acl_len.to_le_bytes().to_vec();
                r.push(4);
                self.command_complete(opcode, &r);
            }
            op::SET_EVENT_MASK
            | op::LE_SET_EVENT_MASK
            | op::WRITE_LE_HOST_SUPPORTED
            | op::LE_SET_SCAN_PARAMETERS => self.command_complete(opcode, &[]),
            op::LE_SET_SCAN_ENABLE => {
                self.scanning = params[0] != 0;
                self.command_complete(opcode, &[]);
            }
            op::LE_CREATE_CONNECTION => {
                self.command_status(opcode, 0);
                self.pending_connect = Some(self.clock + 30);
            }
            op::LE_CREATE_CONNECTION_CANCEL => self.command_complete(opcode, &[]),
            op::DISCONNECT => {
                self.command_status(opcode, 0);
                if let Some(h) = self.connected.take() {
                    let mut p = vec![0];
                    p.extend_from_slice(&h.to_le_bytes());
                    p.push(0x16);
                    self.event(hci::evt::DISCONNECTION_COMPLETE, &p);
                    self.encrypted = false;
                    self.ccc_enabled.set(false);
                }
            }
            op::LE_CONNECTION_UPDATE => {
                self.shared.update_command_seen.set(true);
                self.command_status(opcode, 0);
            }
            op::LE_START_ENCRYPTION => {
                self.shared
                    .enc_start_attempts
                    .set(self.shared.enc_start_attempts.get() + 1);
                self.command_status(opcode, 0);
                let handle = u16::from_le_bytes([params[0], params[1]]);
                let mut ltk = [0u8; 16];
                ltk.copy_from_slice(&params[12..28]);
                let ok = self.periph_ltk == Some(ltk) && params[2..12].iter().all(|&b| b == 0);
                let mut p = vec![if ok { 0 } else { 0x06 }];
                p.extend_from_slice(&handle.to_le_bytes());
                p.push(u8::from(ok));
                self.event(hci::evt::ENCRYPTION_CHANGE, &p);
                self.encrypted = ok;
                if ok {
                    // After encryption a bonded peripheral distributes its identity (SC pairing only).
                    if let Some(st) = &self.smp {
                        if st.ltk.is_some() {
                            let mut irk = vec![smpx::IDENTITY_INFORMATION];
                            irk.extend_from_slice(&[0x11; 16]);
                            self.send_l2cap(l2cap::CID_SMP, &irk);
                            let mut id = vec![smpx::IDENTITY_ADDRESS_INFORMATION, PERIPH_ADDR.kind];
                            id.extend_from_slice(&PERIPH_ADDR.bytes);
                            self.send_l2cap(l2cap::CID_SMP, &id);
                        }
                    }
                }
            }
            _ => self.command_complete(opcode, &[]),
        }
    }

    // ---- GATT server ---------------------------------------------------------------------

    fn att_response(&mut self, pdu: &[u8]) -> Vec<u8> {
        if self.behaviour == Behaviour::HostileAtt {
            // Claims there is more data but always repeats the same group: discovery must not loop forever.
            return vec![
                OP_READ_BY_GROUP_TYPE_RSP,
                6,
                0x01,
                0x00,
                0x05,
                0x00,
                0x00,
                0x18,
            ];
        }
        let err = |op: u8, handle: u16, code: u8| {
            vec![OP_ERROR_RSP, op, handle as u8, (handle >> 8) as u8, code]
        };
        let u16_at = |o: usize| u16::from_le_bytes([pdu[o], pdu[o + 1]]);
        match pdu[0] {
            OP_MTU_REQ => {
                self.mtu = 247;
                vec![OP_MTU_RSP, 0xF7, 0x00]
            }
            OP_READ_BY_GROUP_TYPE_REQ => {
                let (start, _end) = (u16_at(1), u16_at(3));
                // Services: GAP 0x0001-0x0005, HID 0x0010-0x0030, Battery 0x0040-0x0042
                let svcs: [(u16, u16, u16); 3] = [
                    (0x0001, 0x0005, 0x1800),
                    (0x0010, 0x0030, 0x1812),
                    (0x0040, 0x0042, 0x180F),
                ];
                let mut out = vec![OP_READ_BY_GROUP_TYPE_RSP, 6];
                for (s, e, u) in svcs.iter().filter(|(s, _, _)| *s >= start) {
                    out.extend_from_slice(&s.to_le_bytes());
                    out.extend_from_slice(&e.to_le_bytes());
                    out.extend_from_slice(&u.to_le_bytes());
                }
                if out.len() == 2 {
                    err(pdu[0], start, ERR_ATTRIBUTE_NOT_FOUND)
                } else {
                    out
                }
            }
            OP_READ_BY_TYPE_REQ => {
                let (start, end) = (u16_at(1), u16_at(3));
                // Characteristic declarations of the HID service (handle, props, value handle, uuid16).
                let chars: [(u16, u8, u16, u16); 7] = [
                    (0x0011, 0x02, 0x0012, 0x2A4A), // HID information
                    (0x0013, 0x02, 0x0014, 0x2A4B), // report map
                    (0x0015, 0x06, 0x0016, 0x2A4E), // protocol mode
                    (0x0017, 0x04, 0x0018, 0x2A4C), // control point
                    (0x0019, 0x1A, 0x001A, 0x2A4D), // report 1 (input, notify)
                    (0x001D, 0x1A, 0x001E, 0x2A4D), // report 2 (input, notify)
                    (0x0021, 0x0E, 0x0022, 0x2A4D), // report 3 (output)
                ];
                if !(0x0010..=0x0030).contains(&start) && start != 1 {
                    return err(pdu[0], start, ERR_ATTRIBUTE_NOT_FOUND);
                }
                let mut out = vec![OP_READ_BY_TYPE_RSP, 7];
                for (d, p, v, u) in chars.iter().filter(|(d, ..)| *d >= start && *d <= end) {
                    out.extend_from_slice(&d.to_le_bytes());
                    out.push(*p);
                    out.extend_from_slice(&v.to_le_bytes());
                    out.extend_from_slice(&u.to_le_bytes());
                }
                if out.len() == 2 {
                    err(pdu[0], start, ERR_ATTRIBUTE_NOT_FOUND)
                } else {
                    out
                }
            }
            OP_FIND_INFO_REQ => self.find_info(u16_at(1), u16_at(3)),
            OP_READ_REQ | OP_READ_BLOB_REQ => {
                let handle = u16_at(1);
                let offset = if pdu[0] == OP_READ_BLOB_REQ {
                    usize::from(u16_at(3))
                } else {
                    0
                };
                match self.read_attr(handle) {
                    Err(code) => err(pdu[0], handle, code),
                    Ok(value) => {
                        let rest = value.get(offset..).unwrap_or(&[]);
                        let mut out = vec![if pdu[0] == OP_READ_REQ {
                            OP_READ_RSP
                        } else {
                            OP_READ_BLOB_RSP
                        }];
                        out.extend_from_slice(&rest[..rest.len().min(self.mtu - 1)]);
                        out
                    }
                }
            }
            OP_WRITE_REQ => {
                let handle = u16_at(1);
                if !self.encrypted {
                    return err(pdu[0], handle, ERR_INSUFFICIENT_ENCRYPTION);
                }
                match handle {
                    0x001B | 0x001F => {
                        self.ccc_enabled.set(pdu[3..] == [1, 0]);
                        vec![OP_WRITE_RSP]
                    }
                    0x0016 => vec![OP_WRITE_RSP],
                    _ => err(pdu[0], handle, 0x03),
                }
            }
            op => err(op, 0, 0x06),
        }
    }

    /// Descriptors of the HID service: report 1 (CCC 0x1B, Report Reference 0x1C), report 2 (CCC 0x1F,
    /// Report Reference 0x20) and report 3 (Report Reference 0x23).
    fn find_info(&mut self, start: u16, end: u16) -> Vec<u8> {
        let all: [(u16, u16); 5] = [
            (0x001B, 0x2902),
            (0x001C, 0x2908),
            (0x001F, 0x2902),
            (0x0020, 0x2908),
            (0x0023, 0x2908),
        ];
        let mut out = vec![OP_FIND_INFO_RSP, 1];
        for (h, u) in all.iter().filter(|(h, _)| *h >= start && *h <= end) {
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&u.to_le_bytes());
        }
        if out.len() == 2 {
            vec![
                OP_ERROR_RSP,
                OP_FIND_INFO_REQ,
                start as u8,
                (start >> 8) as u8,
                ERR_ATTRIBUTE_NOT_FOUND,
            ]
        } else {
            out
        }
    }

    fn read_attr(&self, handle: u16) -> Result<Vec<u8>, u8> {
        // Everything in the HID service needs encryption.
        if (0x0011..=0x0030).contains(&handle) && !self.encrypted {
            return Err(ERR_INSUFFICIENT_ENCRYPTION);
        }
        match handle {
            0x0012 => Ok(vec![0x11, 0x01, 0x00, 0x02]),
            0x0014 => Ok(report_map()),
            0x001C => Ok(vec![1, 1]), // report 1: ID 1, input
            0x0020 => Ok(vec![2, 1]), // report 2: ID 2, input
            0x0023 => Ok(vec![3, 2]), // report 3: ID 3, output
            _ => Err(0x0A),
        }
    }

    // ---- SMP responder -------------------------------------------------------------------

    fn smp_pdu(&mut self, pdu: &[u8]) {
        let send = |s: &mut Self, p: Vec<u8>| s.send_l2cap(l2cap::CID_SMP, &p);
        match pdu[0] {
            smpx::PAIRING_REQUEST => {
                let init_io = pdu[1];
                let (mut rsp_auth, mut key) = (
                    smpx::AUTH_SECURE_CONNECTIONS | smpx::AUTH_BONDING | smpx::AUTH_MITM,
                    16u8,
                );
                match self.behaviour {
                    Behaviour::LegacyOnly => rsp_auth &= !smpx::AUTH_SECURE_CONNECTIONS,
                    Behaviour::WeakKey => key = 7,
                    _ => {}
                }
                let rsp = vec![
                    smpx::PAIRING_RESPONSE,
                    self.periph_io,
                    0,
                    rsp_auth,
                    key,
                    0,
                    0x02,
                ];
                let method = smpx::select_method(init_io, self.periph_io, true);
                self.smp = Some(SmpState {
                    req: pdu.to_vec(),
                    rsp: rsp.clone(),
                    key: None,
                    pk_a: ([0; 32], [0; 32]),
                    pk_b: ([0; 32], [0; 32]),
                    dh: [0; 32],
                    nb: [0; 16],
                    na: [0; 16],
                    ca_round: [0; 16],
                    round: 0,
                    passkey: 0,
                    method,
                    ltk: None,
                    mac_key: [0; 16],
                });
                send(self, rsp);
            }
            smpx::PAIRING_PUBLIC_KEY => {
                let key = EphemeralKey::generate(&mut self.rng);
                let mut pk_b = key.public();
                let st = self.smp.as_mut().expect("pairing in progress");
                st.pk_a.0.copy_from_slice(&pdu[1..33]);
                st.pk_a.1.copy_from_slice(&pdu[33..65]);
                st.dh = key.dh(&st.pk_a.0, &st.pk_a.1).expect("valid initiator key");
                match self.behaviour {
                    Behaviour::BadPublicKey => pk_b.1[0] ^= 1,
                    Behaviour::ReflectKey => pk_b = st.pk_a,
                    _ => {}
                }
                st.pk_b = key.public();
                st.key = Some(key);
                let mut m = vec![smpx::PAIRING_PUBLIC_KEY];
                m.extend_from_slice(&pk_b.0);
                m.extend_from_slice(&pk_b.1);
                let method = st.method;
                let (pka, pkb) = (st.pk_a.0, st.pk_b.0);
                send(self, m);
                if matches!(
                    method,
                    smpx::Method::JustWorks | smpx::Method::NumericComparison
                ) {
                    let mut nb = [0u8; 16];
                    self.rng.fill(&mut nb);
                    let cb = crypto::f4(&pkb, &pka, &nb, 0);
                    self.smp.as_mut().unwrap().nb = nb;
                    let mut m = vec![smpx::PAIRING_CONFIRM];
                    m.extend_from_slice(&cb);
                    send(self, m);
                }
            }
            smpx::PAIRING_CONFIRM => {
                // Passkey entry round: initiator's commitment Ca arrives first.
                let st = self.smp.as_mut().expect("pairing in progress");
                st.ca_round.copy_from_slice(&pdu[1..17]);
                if st.round == 0 && st.method == smpx::Method::PasskeyDisplay {
                    // The user types the passkey the host displayed (or a wrong one, if the test says so).
                    st.passkey = self
                        .shared
                        .typed_passkey
                        .get()
                        .or(self.shared.displayed_passkey.get())
                        .unwrap_or(0);
                }
                let round = st.round;
                let bit = ((st.passkey >> round) & 1) as u8;
                let ri = 0x80 | bit;
                let mut nb = [0u8; 16];
                self.rng.fill(&mut nb);
                let st = self.smp.as_mut().unwrap();
                st.nb = nb;
                let cb = crypto::f4(&st.pk_b.0, &st.pk_a.0, &nb, ri);
                let mut m = vec![smpx::PAIRING_CONFIRM];
                m.extend_from_slice(&cb);
                send(self, m);
            }
            smpx::PAIRING_RANDOM => {
                let st = self.smp.as_mut().expect("pairing in progress");
                st.na.copy_from_slice(&pdu[1..17]);
                if matches!(st.method, smpx::Method::PasskeyDisplay) {
                    let bit = ((st.passkey >> st.round) & 1) as u8;
                    let ri = 0x80 | bit;
                    let expect = crypto::f4(&st.pk_a.0, &st.pk_b.0, &st.na, ri);
                    if expect != st.ca_round {
                        send(
                            self,
                            vec![smpx::PAIRING_FAILED, smpx::reason::CONFIRM_VALUE_FAILED],
                        );
                        self.smp = None;
                        return;
                    }
                    st.round += 1;
                }
                if st.method == smpx::Method::NumericComparison {
                    self.shared
                        .peripheral_numeric
                        .set(Some(crypto::g2(&st.pk_a.0, &st.pk_b.0, &st.na, &st.nb)));
                }
                let mut m = vec![smpx::PAIRING_RANDOM];
                m.extend_from_slice(&st.nb);
                send(self, m);
            }
            smpx::PAIRING_DHKEY_CHECK => {
                let st = self.smp.as_mut().expect("pairing in progress");
                let (a, b) = (addr7(&HOST_ADDR), addr7(&PERIPH_ADDR));
                let (mac, ltk) = crypto::f5(&st.dh, &st.na, &st.nb, &a, &b);
                let r = if matches!(st.method, smpx::Method::PasskeyDisplay) {
                    passkey_r(st.passkey)
                } else {
                    [0; 16]
                };
                let io_a = [st.req[1], st.req[2], st.req[3]];
                let io_b = [st.rsp[1], st.rsp[2], st.rsp[3]];
                let ea = crypto::f6(&mac, &st.na, &st.nb, &r, &io_a, &a, &b);
                if ea[..] != pdu[1..17] {
                    send(
                        self,
                        vec![smpx::PAIRING_FAILED, smpx::reason::DHKEY_CHECK_FAILED],
                    );
                    self.smp = None;
                    return;
                }
                let eb = crypto::f6(&mac, &st.nb, &st.na, &r, &io_b, &b, &a);
                st.ltk = Some(ltk);
                st.mac_key = mac;
                self.periph_ltk = Some(ltk);
                let mut m = vec![smpx::PAIRING_DHKEY_CHECK];
                m.extend_from_slice(&eb);
                send(self, m);
            }
            _ => {}
        }
    }
}

fn addr7(a: &Address) -> [u8; 7] {
    let mut o = [0u8; 7];
    o[0] = a.kind;
    o[1..].copy_from_slice(&a.bytes);
    o
}

fn passkey_r(p: u32) -> [u8; 16] {
    let mut r = [0u8; 16];
    r[..4].copy_from_slice(&p.to_le_bytes());
    r
}

impl Transport for Sim {
    fn send_command(&mut self, packet: &[u8]) -> Result<(), HostError> {
        self.handle_command(packet);
        Ok(())
    }

    fn send_acl(&mut self, packet: &[u8]) -> Result<(), HostError> {
        let (h, data) = hci::parse_acl(packet).ok_or(HostError::Malformed)?;
        // Return the buffer credit.
        let mut p = vec![1u8];
        p.extend_from_slice(&h.handle.to_le_bytes());
        p.extend_from_slice(&1u16.to_le_bytes());
        self.event(hci::evt::NUMBER_OF_COMPLETED_PACKETS, &p);
        if let Some(frame) = self.reasm.push(&h, data) {
            match frame.cid {
                l2cap::CID_ATT => {
                    let rsp = self.att_response(&frame.payload);
                    self.send_l2cap(l2cap::CID_ATT, &rsp);
                }
                l2cap::CID_SMP => self.smp_pdu(&frame.payload),
                l2cap::CID_LE_SIGNALING => {
                    self.shared.param_update_response.set(Some(
                        frame.payload.first()
                            == Some(&l2cap::sig::CONNECTION_PARAMETER_UPDATE_RESPONSE)
                            && frame.payload.get(4) == Some(&0),
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn recv(&mut self, timeout_ms: u64) -> Option<Packet> {
        if let Some(t) = self.pending_connect {
            if self.clock >= t {
                self.pending_connect = None;
                let handle = self.next_handle;
                self.next_handle += 1;
                self.connected = Some(handle);
                let mut p = vec![hci::le::CONNECTION_COMPLETE, 0];
                p.extend_from_slice(&handle.to_le_bytes());
                p.push(0); // role: central
                p.push(PERIPH_ADDR.kind);
                p.extend_from_slice(&PERIPH_ADDR.bytes);
                for v in [12u16, 0, 600] {
                    p.extend_from_slice(&v.to_le_bytes());
                }
                p.push(0);
                self.event(hci::evt::LE_META, &p);
            }
        }
        if self.scanning && self.discoverable && self.adv_sent < 3 && self.queue.is_empty() {
            self.adv_sent += 1;
            let mut data = vec![
                2,
                hci::ad::FLAGS,
                0x06,
                3,
                hci::ad::COMPLETE_UUID16,
                0x12,
                0x18,
                6,
                hci::ad::COMPLETE_NAME,
            ];
            data.extend_from_slice(b"SimKb");
            data.extend_from_slice(&[3, hci::ad::APPEARANCE, 0xC1, 0x03]);
            let mut p = vec![hci::le::ADVERTISING_REPORT, 1, 0, PERIPH_ADDR.kind];
            p.extend_from_slice(&PERIPH_ADDR.bytes);
            p.push(data.len() as u8);
            p.extend_from_slice(&data);
            p.push(0xC0); // rssi -64
            self.event(hci::evt::LE_META, &p);
        }
        if self.unsolicited_garbage && self.queue.is_empty() {
            self.unsolicited_garbage = false;
            self.queue
                .push_back(Packet::Event(vec![0x3E, 200, 0x02, 0xFF]));
            self.queue.push_back(Packet::Acl(vec![0xFF; 7]));
            self.queue
                .push_back(Packet::Acl(vec![0x40, 0x10, 0x03, 0x00, 1, 2, 3]));
        }
        match self.queue.pop_front() {
            Some(p) => Some(p),
            None => {
                self.clock += timeout_ms.max(1);
                None
            }
        }
    }

    fn now_ms(&mut self) -> u64 {
        self.clock
    }
}
