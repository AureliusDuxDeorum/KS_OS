// SPDX-License-Identifier: Apache-2.0
//! Security Manager Protocol (Core Spec Vol 3, Part H): LE Secure Connections pairing as the
//! initiator.
//!
//! Supports Just Works, Numeric Comparison and Passkey Entry. Hardening choices:
//! * LE Secure Connections only (legacy pairing is weak and is refused);
//! * the peer's P-256 public key is validated on the curve and must differ from ours;
//! * a 16-byte encryption key is required (no KNOB-style downgrade);
//! * every peer value is checked with constant-time-agnostic equality *after* the full
//!   protocol step, and any mismatch aborts with a `Pairing Failed` to the peer.

use crate::crypto::{self, EphemeralKey};
use crate::gatt::Error as LinkError;
use crate::{Address, Rng};
use alloc::vec::Vec;
use zeroize::Zeroize;

pub const PAIRING_REQUEST: u8 = 0x01;
pub const PAIRING_RESPONSE: u8 = 0x02;
pub const PAIRING_CONFIRM: u8 = 0x03;
pub const PAIRING_RANDOM: u8 = 0x04;
pub const PAIRING_FAILED: u8 = 0x05;
pub const IDENTITY_INFORMATION: u8 = 0x08;
pub const IDENTITY_ADDRESS_INFORMATION: u8 = 0x09;
pub const PAIRING_PUBLIC_KEY: u8 = 0x0C;
pub const PAIRING_DHKEY_CHECK: u8 = 0x0D;

/// Pairing Failed reasons.
pub mod reason {
    pub const PASSKEY_ENTRY_FAILED: u8 = 0x01;
    pub const CONFIRM_VALUE_FAILED: u8 = 0x04;
    pub const PAIRING_NOT_SUPPORTED: u8 = 0x05;
    pub const ENCRYPTION_KEY_SIZE: u8 = 0x06;
    pub const UNSPECIFIED: u8 = 0x08;
    pub const DHKEY_CHECK_FAILED: u8 = 0x0B;
    pub const NUMERIC_COMPARISON_FAILED: u8 = 0x0C;
    pub const INVALID_PARAMETERS: u8 = 0x0A;
}

/// IO capabilities.
pub mod io {
    pub const DISPLAY_ONLY: u8 = 0;
    pub const DISPLAY_YES_NO: u8 = 1;
    pub const KEYBOARD_ONLY: u8 = 2;
    pub const NO_INPUT_NO_OUTPUT: u8 = 3;
    pub const KEYBOARD_DISPLAY: u8 = 4;
}

pub const AUTH_BONDING: u8 = 0x01;
pub const AUTH_MITM: u8 = 0x04;
pub const AUTH_SECURE_CONNECTIONS: u8 = 0x08;

const KEY_DIST_ID: u8 = 0x02;

/// The way the two devices authenticate each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    JustWorks,
    NumericComparison,
    /// The initiator (us) shows the passkey; the peer types it.
    PasskeyDisplay,
    /// The peer shows the passkey; the user enters it here.
    PasskeyInput,
    /// Both sides enter the same passkey.
    PasskeyBothInput,
}

/// Selects the association model (Core Spec Vol 3, Part H, Table 2.8, LE Secure Connections).
pub fn select_method(initiator_io: u8, responder_io: u8, mitm: bool) -> Method {
    use io::*;
    if !mitm || initiator_io == NO_INPUT_NO_OUTPUT || responder_io == NO_INPUT_NO_OUTPUT {
        return Method::JustWorks;
    }
    let (i, r) = (initiator_io, responder_io);
    let init_displays_resp_inputs = matches!(i, DISPLAY_ONLY | DISPLAY_YES_NO | KEYBOARD_DISPLAY)
        && matches!(r, KEYBOARD_ONLY | KEYBOARD_DISPLAY);
    match (i, r) {
        (DISPLAY_YES_NO | KEYBOARD_DISPLAY, DISPLAY_YES_NO | KEYBOARD_DISPLAY) => {
            Method::NumericComparison
        }
        (DISPLAY_ONLY, DISPLAY_ONLY | DISPLAY_YES_NO) => Method::JustWorks,
        (KEYBOARD_ONLY, KEYBOARD_ONLY) => Method::PasskeyBothInput,
        (KEYBOARD_ONLY | KEYBOARD_DISPLAY, DISPLAY_ONLY | DISPLAY_YES_NO) => Method::PasskeyInput,
        _ if init_displays_resp_inputs => Method::PasskeyDisplay,
        (KEYBOARD_ONLY, KEYBOARD_DISPLAY) => Method::PasskeyInput,
        _ => Method::JustWorks,
    }
}

/// A synchronous SMP channel (L2CAP fixed channel 6) to the peer.
pub trait SmpLink {
    fn send(&mut self, pdu: &[u8]) -> Result<(), LinkError>;
    /// Waits for the next SMP PDU.
    fn recv(&mut self, timeout_ms: u64) -> Result<Vec<u8>, LinkError>;
}

/// The user-facing side of pairing.
pub trait PairingUi {
    fn display_passkey(&mut self, passkey: u32);
    /// Numeric comparison: show `value` and return whether the user confirms it matches.
    fn confirm_numeric(&mut self, value: u32) -> bool;
    /// Passkey entry: ask the user for the six digits shown by the peer.
    fn enter_passkey(&mut self) -> Option<u32>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum PairError {
    Link(LinkError),
    /// The peer sent Pairing Failed with this reason.
    PeerFailed(u8),
    /// We aborted (reason sent to the peer).
    Aborted(u8, &'static str),
}

impl From<LinkError> for PairError {
    fn from(e: LinkError) -> Self {
        Self::Link(e)
    }
}

pub struct PairingConfig {
    pub io_capability: u8,
    pub bonding: bool,
    pub local: Address,
    pub peer: Address,
}

/// Result of a successful pairing.
pub struct Keys {
    pub ltk: [u8; 16],
    /// Whether MITM protection was achieved (everything except Just Works).
    pub authenticated: bool,
    pub method: Method,
    pub peer_irk: Option<[u8; 16]>,
    pub peer_identity: Option<Address>,
}

impl Drop for Keys {
    fn drop(&mut self) {
        self.ltk.zeroize();
        if let Some(irk) = self.peer_irk.as_mut() {
            irk.zeroize();
        }
    }
}

fn addr7(a: &Address) -> [u8; 7] {
    let mut o = [0u8; 7];
    o[0] = a.kind;
    o[1..].copy_from_slice(&a.bytes);
    o
}

fn expect(link: &mut impl SmpLink, code: u8, len: usize) -> Result<Vec<u8>, PairError> {
    let pdu = link.recv(5000)?;
    match pdu.first() {
        Some(&PAIRING_FAILED) => Err(PairError::PeerFailed(
            pdu.get(1).copied().unwrap_or(reason::UNSPECIFIED),
        )),
        Some(&c) if c == code && pdu.len() == len => Ok(pdu),
        _ => {
            let _ = link.send(&[PAIRING_FAILED, reason::UNSPECIFIED]);
            Err(PairError::Aborted(
                reason::UNSPECIFIED,
                "unexpected or malformed SMP PDU",
            ))
        }
    }
}

fn abort<T>(link: &mut impl SmpLink, reason: u8, why: &'static str) -> Result<T, PairError> {
    let _ = link.send(&[PAIRING_FAILED, reason]);
    Err(PairError::Aborted(reason, why))
}

fn random16(rng: &mut dyn Rng) -> [u8; 16] {
    let mut r = [0u8; 16];
    rng.fill(&mut r);
    r
}

fn passkey_r(passkey: u32) -> [u8; 16] {
    let mut r = [0u8; 16];
    r[..4].copy_from_slice(&passkey.to_le_bytes());
    r
}

/// Runs LE Secure Connections pairing to completion and returns the Long Term Key.
pub fn pair(
    link: &mut impl SmpLink,
    ui: &mut impl PairingUi,
    rng: &mut dyn Rng,
    cfg: &PairingConfig,
) -> Result<Keys, PairError> {
    // --- Phase 1: feature exchange ---
    let auth_req = AUTH_SECURE_CONNECTIONS | AUTH_MITM | if cfg.bonding { AUTH_BONDING } else { 0 };
    let req = [
        PAIRING_REQUEST,
        cfg.io_capability,
        0,
        auth_req,
        16,
        0x00,
        KEY_DIST_ID,
    ];
    link.send(&req)?;
    let rsp = expect(link, PAIRING_RESPONSE, 7)?;
    let (peer_io, peer_oob, peer_auth, peer_key_size, peer_init_dist, peer_resp_dist) =
        (rsp[1], rsp[2], rsp[3], rsp[4], rsp[5], rsp[6]);
    if peer_auth & AUTH_SECURE_CONNECTIONS == 0 {
        return abort(
            link,
            reason::PAIRING_NOT_SUPPORTED,
            "peer does not support LE Secure Connections",
        );
    }
    if peer_key_size < 16 {
        return abort(
            link,
            reason::ENCRYPTION_KEY_SIZE,
            "peer offers a weak encryption key size",
        );
    }
    // Key-distribution fields are tolerated whatever the peer sets: we distribute nothing and
    // only act on the identity information we asked for.
    let _ = (peer_init_dist, peer_resp_dist);
    if peer_oob != 0 || peer_io > io::KEYBOARD_DISPLAY {
        return abort(
            link,
            reason::INVALID_PARAMETERS,
            "unsupported pairing response parameters",
        );
    }
    let mitm = auth_req & AUTH_MITM != 0 && peer_auth & AUTH_MITM != 0;
    let method = select_method(cfg.io_capability, peer_io, mitm);

    // --- Phase 2: public key exchange ---
    let key = EphemeralKey::generate(rng);
    let (pk_ax, pk_ay) = key.public();
    let mut pk_msg = alloc::vec![PAIRING_PUBLIC_KEY];
    pk_msg.extend_from_slice(&pk_ax);
    pk_msg.extend_from_slice(&pk_ay);
    link.send(&pk_msg)?;
    let peer_pk = expect(link, PAIRING_PUBLIC_KEY, 65)?;
    let (mut pk_bx, mut pk_by) = ([0u8; 32], [0u8; 32]);
    pk_bx.copy_from_slice(&peer_pk[1..33]);
    pk_by.copy_from_slice(&peer_pk[33..65]);
    if pk_bx == pk_ax {
        return abort(
            link,
            reason::INVALID_PARAMETERS,
            "peer reflected our public key",
        );
    }
    let Ok(mut dhkey) = key.dh(&pk_bx, &pk_by) else {
        return abort(
            link,
            reason::INVALID_PARAMETERS,
            "peer public key is not on the curve",
        );
    };

    // --- Phase 2, stage 1: authentication ---
    let (na, nb, ra, rb);
    let mut authenticated = true;
    match method {
        Method::JustWorks | Method::NumericComparison => {
            let cb = expect(link, PAIRING_CONFIRM, 17)?;
            let n_a = random16(rng);
            let mut m = alloc::vec![PAIRING_RANDOM];
            m.extend_from_slice(&n_a);
            link.send(&m)?;
            let n_b_pdu = expect(link, PAIRING_RANDOM, 17)?;
            let mut n_b = [0u8; 16];
            n_b.copy_from_slice(&n_b_pdu[1..]);
            // Cb = f4(PKb, PKa, Nb, 0)
            if crypto::f4(&pk_bx, &pk_ax, &n_b, 0)[..] != cb[1..] {
                return abort(
                    link,
                    reason::CONFIRM_VALUE_FAILED,
                    "peer confirm value mismatch",
                );
            }
            if method == Method::NumericComparison {
                let v = crypto::g2(&pk_ax, &pk_bx, &n_a, &n_b);
                if !ui.confirm_numeric(v) {
                    return abort(
                        link,
                        reason::NUMERIC_COMPARISON_FAILED,
                        "user rejected the numeric comparison",
                    );
                }
            } else {
                authenticated = false;
            }
            (na, nb, ra, rb) = (n_a, n_b, [0u8; 16], [0u8; 16]);
        }
        Method::PasskeyDisplay | Method::PasskeyInput | Method::PasskeyBothInput => {
            let passkey = if method == Method::PasskeyDisplay {
                let mut b = [0u8; 4];
                rng.fill(&mut b);
                let p = u32::from_le_bytes(b) % 1_000_000;
                ui.display_passkey(p);
                p
            } else {
                match ui.enter_passkey() {
                    Some(p) if p < 1_000_000 => p,
                    _ => return abort(link, reason::PASSKEY_ENTRY_FAILED, "no passkey entered"),
                }
            };
            let r = passkey_r(passkey);
            let (mut last_na, mut last_nb) = ([0u8; 16], [0u8; 16]);
            for round in 0..20 {
                let bit = ((passkey >> round) & 1) as u8;
                let mut ri = [0u8; 16];
                ri[0] = 0x80 | bit;
                let n_ai = random16(rng);
                let ca = crypto::f4(&pk_ax, &pk_bx, &n_ai, ri[0]);
                let mut m = alloc::vec![PAIRING_CONFIRM];
                m.extend_from_slice(&ca);
                link.send(&m)?;
                let cb = expect(link, PAIRING_CONFIRM, 17)?;
                let mut m = alloc::vec![PAIRING_RANDOM];
                m.extend_from_slice(&n_ai);
                link.send(&m)?;
                let nb_pdu = expect(link, PAIRING_RANDOM, 17)?;
                let mut n_bi = [0u8; 16];
                n_bi.copy_from_slice(&nb_pdu[1..]);
                if crypto::f4(&pk_bx, &pk_ax, &n_bi, ri[0])[..] != cb[1..] {
                    return abort(
                        link,
                        reason::PASSKEY_ENTRY_FAILED,
                        "passkey confirm mismatch (wrong passkey?)",
                    );
                }
                (last_na, last_nb) = (n_ai, n_bi);
            }
            (na, nb, ra, rb) = (last_na, last_nb, r, r);
        }
    }

    // --- Phase 2, stage 2: long term key and DHKey checks ---
    let (a, b) = (addr7(&cfg.local), addr7(&cfg.peer));
    let (mut mac_key, ltk) = crypto::f5(&dhkey, &na, &nb, &a, &b);
    dhkey.zeroize();
    let io_a = [cfg.io_capability, 0, auth_req];
    let io_b = [peer_io, peer_oob, peer_auth];
    let ea = crypto::f6(&mac_key, &na, &nb, &rb, &io_a, &a, &b);
    let mut m = alloc::vec![PAIRING_DHKEY_CHECK];
    m.extend_from_slice(&ea);
    link.send(&m)?;
    let eb_pdu = expect(link, PAIRING_DHKEY_CHECK, 17)?;
    let eb = crypto::f6(&mac_key, &nb, &na, &ra, &io_b, &b, &a);
    mac_key.zeroize();
    if eb[..] != eb_pdu[1..] {
        return abort(link, reason::DHKEY_CHECK_FAILED, "peer DHKey check failed");
    }
    Ok(Keys {
        ltk,
        authenticated,
        method,
        peer_irk: None,
        peer_identity: None,
    })
}

/// After encryption is on, reads the identity keys the peer distributes (if any) for up to `wait_ms`.
pub fn receive_identity(link: &mut impl SmpLink, keys: &mut Keys, wait_ms: u64) {
    for _ in 0..4 {
        let Ok(pdu) = link.recv(wait_ms) else { return };
        match (pdu.first(), pdu.len()) {
            (Some(&IDENTITY_INFORMATION), 17) => {
                let mut irk = [0u8; 16];
                irk.copy_from_slice(&pdu[1..]);
                keys.peer_irk = Some(irk);
            }
            (Some(&IDENTITY_ADDRESS_INFORMATION), 8) => {
                let mut bytes = [0u8; 6];
                bytes.copy_from_slice(&pdu[2..8]);
                keys.peer_identity = Some(Address::new(pdu[1], bytes));
            }
            _ => {}
        }
    }
}
