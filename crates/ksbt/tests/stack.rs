// SPDX-License-Identifier: Apache-2.0
//! End-to-end tests of the Bluetooth host stack against the simulated controller and keyboard.

mod common;

use common::*;
use ksbt::bond::{Bond, BondStore};
use ksbt::gatt::{self, AttLink};
use ksbt::hci::{self, parse_adv_data};
use ksbt::hogp::HogpDevice;
use ksbt::host::Host;
use ksbt::smp::{self, io, Method, PairError, PairingConfig, PairingUi};
use std::cell::Cell;
use std::rc::Rc;

struct Ui {
    shared: Rc<Shared>,
    typed_by_user: Option<u32>,
    confirm: bool,
    shown_numeric: Cell<Option<u32>>,
}

impl PairingUi for Ui {
    fn display_passkey(&mut self, passkey: u32) {
        self.shared.displayed_passkey.set(Some(passkey));
    }
    fn confirm_numeric(&mut self, value: u32) -> bool {
        self.shown_numeric.set(Some(value));
        self.confirm
    }
    fn enter_passkey(&mut self) -> Option<u32> {
        self.typed_by_user
    }
}

fn setup(behaviour: Behaviour, periph_io: u8) -> (Host<Sim>, Rc<Shared>) {
    let shared = Rc::new(Shared::default());
    let mut sim = Sim::new(shared.clone());
    sim.behaviour = behaviour;
    sim.periph_io = periph_io;
    let mut host = Host::new(sim);
    host.init().expect("controller init");
    (host, shared)
}

fn ui(shared: &Rc<Shared>) -> Ui {
    Ui {
        shared: shared.clone(),
        typed_by_user: None,
        confirm: true,
        shown_numeric: Cell::new(None),
    }
}

fn cfg(io_capability: u8) -> PairingConfig {
    PairingConfig {
        io_capability,
        bonding: true,
        local: HOST_ADDR,
        peer: PERIPH_ADDR,
    }
}

fn connect(host: &mut Host<Sim>) -> u16 {
    host.connect(PERIPH_ADDR, 3000).expect("connect")
}

#[test]
fn controller_init_reports_what_the_controller_said() {
    let (host, _) = setup(Behaviour::Normal, io::NO_INPUT_NO_OUTPUT);
    let info = host.info.unwrap();
    assert_eq!(info.address, HOST_ADDR);
    assert_eq!((info.acl_len, info.acl_buffers), (27, 4));
    assert_eq!(
        (
            info.version.hci_version,
            info.version.manufacturer,
            info.version.lmp_subversion
        ),
        (0x0B, 0x008F, 0x7856)
    );
}

#[test]
fn scan_finds_a_discoverable_hid_keyboard() {
    let (mut host, _) = setup(Behaviour::Normal, io::NO_INPUT_NO_OUTPUT);
    let mut found = Vec::new();
    host.scan(500, |r| found.push((r.address, parse_adv_data(&r.data))))
        .unwrap();
    assert!(!found.is_empty());
    let (addr, info) = &found[0];
    assert_eq!(*addr, PERIPH_ADDR);
    assert!(info.discoverable() && info.is_hid());
    assert_eq!(info.name_lossy(), "SimKb");
    assert_eq!(info.appearance, Some(0x03C1));
}

#[test]
fn hid_service_requires_encryption_before_pairing() {
    let (mut host, _) = setup(Behaviour::Normal, io::NO_INPUT_NO_OUTPUT);
    let h = connect(&mut host);
    let mut link = host.att(h);
    // Discovery of services works unencrypted, reading the report map does not.
    let services = gatt::discover_services(&mut link).unwrap();
    assert_eq!(
        services
            .iter()
            .filter_map(|s| s.uuid.short())
            .collect::<Vec<_>>(),
        vec![0x1800, 0x1812, 0x180F]
    );
    assert_eq!(
        gatt::read_long(&mut link, 0x0014, 23, 4096),
        Err(gatt::Error::Att(0x0F))
    );
}

#[test]
fn just_works_pairing_then_hid_input() {
    let (mut host, shared) = setup(Behaviour::Normal, io::NO_INPUT_NO_OUTPUT);
    let h = connect(&mut host);
    let mut rng = TestRng(42);
    let mut u = ui(&shared);
    let keys = {
        let mut link = host.smp(h);
        smp::pair(&mut link, &mut u, &mut rng, &cfg(io::DISPLAY_ONLY)).expect("pairing")
    };
    assert_eq!(keys.method, Method::JustWorks);
    assert!(!keys.authenticated, "Just Works is unauthenticated");
    host.start_encryption(h, keys.ltk)
        .expect("encryption with the derived LTK");
    assert!(host.is_encrypted(h));

    let mtu = gatt::exchange_mtu(&mut host.att(h), 247);
    assert_eq!(mtu, 247);
    let dev = HogpDevice::setup(&mut host.att(h), mtu).expect("HOGP setup");
    assert!(dev.is_keyboard() && dev.is_pointer());
    assert!(dev.descriptor.uses_report_ids);
    assert_eq!(dev.reports.iter().filter(|r| r.kind == 1).count(), 2);
    assert!(
        host.transport.ccc_enabled.get(),
        "input report notifications were enabled"
    );

    // A keypress arrives as a notification and decodes through the report map.
    host.transport.press_key(0x02, 0x13);
    let (_, handle, value) = host.wait_notification(100).expect("notification");
    let input = dev.decode(handle, &value).expect("input report");
    assert_eq!(
        (input.modifiers, input.keys.as_slice()),
        (0x02, &[0x13][..])
    );
    // Mouse motion arrives on the second report.
    host.transport.move_mouse(0b001, 5, -3);
    let (_, handle, value) = host.wait_notification(100).expect("notification");
    let input = dev.decode(handle, &value).expect("input report");
    assert_eq!((input.buttons, input.dx, input.dy), (1, 5, -3));
}

#[test]
fn identity_keys_are_collected_after_encryption() {
    let (mut host, shared) = setup(Behaviour::Normal, io::NO_INPUT_NO_OUTPUT);
    let h = connect(&mut host);
    let mut keys = smp::pair(
        &mut host.smp(h),
        &mut ui(&shared),
        &mut TestRng(7),
        &cfg(io::DISPLAY_ONLY),
    )
    .unwrap();
    host.start_encryption(h, keys.ltk).unwrap();
    smp::receive_identity(&mut host.smp(h), &mut keys, 50);
    assert_eq!(keys.peer_irk, Some([0x11; 16]));
    assert_eq!(keys.peer_identity, Some(PERIPH_ADDR));
}

#[test]
fn passkey_entry_when_the_keyboard_types_what_we_display() {
    let (mut host, shared) = setup(Behaviour::Normal, io::KEYBOARD_ONLY);
    let h = connect(&mut host);
    let mut u = ui(&shared);
    let keys = smp::pair(
        &mut host.smp(h),
        &mut u,
        &mut TestRng(99),
        &cfg(io::DISPLAY_ONLY),
    )
    .expect("passkey pairing");
    assert_eq!(keys.method, Method::PasskeyDisplay);
    assert!(keys.authenticated, "passkey entry protects against MITM");
    let shown = shared
        .displayed_passkey
        .get()
        .expect("passkey was displayed");
    assert!(shown < 1_000_000);
    host.start_encryption(h, keys.ltk).expect("encryption");
}

#[test]
fn wrong_passkey_is_rejected() {
    let (mut host, shared) = setup(Behaviour::Normal, io::KEYBOARD_ONLY);
    shared.typed_passkey.set(Some(123_456)); // the user mistypes (and it differs from what is displayed)
    let h = connect(&mut host);
    let r = smp::pair(
        &mut host.smp(h),
        &mut ui(&shared),
        &mut TestRng(5),
        &cfg(io::DISPLAY_ONLY),
    );
    assert!(
        matches!(
            r,
            Err(PairError::PeerFailed(_)) | Err(PairError::Aborted(..))
        ),
        "pairing must fail with a wrong passkey, got {:?}",
        r.as_ref().err()
    );
}

#[test]
fn numeric_comparison_shows_the_same_value_on_both_sides() {
    let (mut host, shared) = setup(Behaviour::Normal, io::DISPLAY_YES_NO);
    let h = connect(&mut host);
    let u = ui(&shared);
    let mut u2 = Ui { confirm: true, ..u };
    let keys = smp::pair(
        &mut host.smp(h),
        &mut u2,
        &mut TestRng(11),
        &cfg(io::DISPLAY_YES_NO),
    )
    .expect("numeric comparison");
    assert_eq!(keys.method, Method::NumericComparison);
    assert!(keys.authenticated);
    assert_eq!(
        u2.shown_numeric.get(),
        shared.peripheral_numeric.get(),
        "both devices display the same six digits"
    );
    assert!(u2.shown_numeric.get().is_some());
}

#[test]
fn numeric_comparison_rejected_by_the_user() {
    let (mut host, shared) = setup(Behaviour::Normal, io::DISPLAY_YES_NO);
    let h = connect(&mut host);
    let mut u = ui(&shared);
    u.confirm = false;
    let r = smp::pair(
        &mut host.smp(h),
        &mut u,
        &mut TestRng(11),
        &cfg(io::DISPLAY_YES_NO),
    );
    assert!(matches!(
        r,
        Err(PairError::Aborted(
            smp::reason::NUMERIC_COMPARISON_FAILED,
            _
        ))
    ));
}

#[test]
fn hostile_pairing_peers_are_refused() {
    for (behaviour, label) in [
        (Behaviour::BadPublicKey, "off-curve public key"),
        (Behaviour::LegacyOnly, "legacy pairing"),
        (Behaviour::WeakKey, "7-byte key"),
        (Behaviour::ReflectKey, "reflected public key"),
    ] {
        let (mut host, shared) = setup(behaviour, io::NO_INPUT_NO_OUTPUT);
        let h = connect(&mut host);
        let r = smp::pair(
            &mut host.smp(h),
            &mut ui(&shared),
            &mut TestRng(3),
            &cfg(io::DISPLAY_ONLY),
        );
        assert!(
            matches!(r, Err(PairError::Aborted(..))),
            "{label}: expected the pairing to be refused, got {:?}",
            r.as_ref().err()
        );
    }
}

#[test]
fn reconnect_re_encrypts_with_the_stored_bond_without_pairing() {
    let (mut host, shared) = setup(Behaviour::Normal, io::NO_INPUT_NO_OUTPUT);
    let h = connect(&mut host);
    let mut keys = smp::pair(
        &mut host.smp(h),
        &mut ui(&shared),
        &mut TestRng(1),
        &cfg(io::DISPLAY_ONLY),
    )
    .unwrap();
    host.start_encryption(h, keys.ltk).unwrap();
    smp::receive_identity(&mut host.smp(h), &mut keys, 50);

    let mut store = BondStore::default();
    store.upsert(Bond {
        address: PERIPH_ADDR,
        ltk: keys.ltk,
        authenticated: keys.authenticated,
        irk: keys.peer_irk,
        kind: 1,
    });
    let persisted = store.to_bytes();

    host.disconnect(h);
    while host.is_connected(h) {
        host.pump(10);
    }
    // "Reboot": reload the bonds from their serialised form and reconnect.
    let store = BondStore::from_bytes(&persisted);
    let bond = store
        .find(&PERIPH_ADDR)
        .expect("bond survives serialisation");
    let attempts = shared.enc_start_attempts.get();
    let h2 = connect(&mut host);
    host.start_encryption(h2, bond.ltk)
        .expect("encryption with the stored LTK, no pairing");
    assert_eq!(shared.enc_start_attempts.get(), attempts + 1);
    let dev = HogpDevice::setup(&mut host.att(h2), 23).expect("HID service available again");
    assert!(dev.is_keyboard());
}

#[test]
fn wrong_stored_key_fails_to_encrypt() {
    let (mut host, shared) = setup(Behaviour::Normal, io::NO_INPUT_NO_OUTPUT);
    let h = connect(&mut host);
    let keys = smp::pair(
        &mut host.smp(h),
        &mut ui(&shared),
        &mut TestRng(1),
        &cfg(io::DISPLAY_ONLY),
    )
    .unwrap();
    let mut bad = keys.ltk;
    bad[0] ^= 1;
    assert!(host.start_encryption(h, bad).is_err());
    assert!(!host.is_encrypted(h));
}

#[test]
fn connection_parameter_update_requests_are_answered() {
    let (mut host, shared) = setup(Behaviour::Normal, io::NO_INPUT_NO_OUTPUT);
    let _h = connect(&mut host);
    host.transport.request_parameter_update();
    host.pump(10);
    assert_eq!(shared.param_update_response.get(), Some(true));
    assert!(
        shared.update_command_seen.get(),
        "the controller was told to apply the parameters"
    );
}

#[test]
fn hostile_att_server_cannot_make_discovery_loop() {
    let (mut host, _) = setup(Behaviour::HostileAtt, io::NO_INPUT_NO_OUTPUT);
    let h = connect(&mut host);
    let mut link = host.att(h);
    let r = gatt::discover_services(&mut link);
    // Bounded: it terminates (with whatever it collected) instead of spinning.
    assert!(r.is_ok() || r.is_err());
    assert!(link.request(&[0x0A, 0x01, 0x00]).is_ok());
}

#[test]
fn garbage_from_the_controller_is_survived() {
    let (mut host, _) = setup(Behaviour::Normal, io::NO_INPUT_NO_OUTPUT);
    host.transport.unsolicited_garbage = true;
    let h = connect(&mut host);
    for _ in 0..10 {
        host.pump(5);
    }
    assert!(host.is_connected(h));
}

#[test]
fn bond_store_roundtrip_and_corruption() {
    let mut store = BondStore::default();
    for i in 0..3u8 {
        store.upsert(Bond {
            address: ksbt::Address::new(0, [i, 1, 2, 3, 4, 5]),
            ltk: [i; 16],
            authenticated: i == 1,
            irk: (i == 2).then_some([0xAA; 16]),
            kind: i,
        });
    }
    let bytes = store.to_bytes();
    let back = BondStore::from_bytes(&bytes);
    assert_eq!(back.bonds.len(), 3);
    assert_eq!(
        back.find(&ksbt::Address::new(0, [2, 1, 2, 3, 4, 5]))
            .unwrap()
            .irk,
        Some([0xAA; 16])
    );
    // Any single-byte corruption or truncation yields an empty store, never a panic or wrong keys.
    for i in 0..bytes.len() {
        let mut c = bytes.clone();
        c[i] ^= 0x5A;
        assert!(
            BondStore::from_bytes(&c).bonds.is_empty(),
            "corruption at byte {i} must be detected"
        );
    }
    for n in 0..bytes.len() {
        assert!(BondStore::from_bytes(&bytes[..n]).bonds.is_empty());
    }
    // Debug output never contains key material.
    assert!(!format!("{:?}", back.bonds[0]).contains("ltk"));
}

#[test]
fn resolvable_private_addresses_resolve_with_the_identity_resolving_key() {
    use ksbt::crypto::{ah, resolves};
    // Core Spec Vol 3, Part H, Appendix D.7: ah() sample data.
    let le = |s: &str| -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .rev()
            .collect()
    };
    let irk: [u8; 16] = le("ec0234a357c8ad05341010a60a397d9b").try_into().unwrap();
    let prand: [u8; 3] = le("708194").try_into().unwrap();
    assert_eq!(ah(&irk, &prand).to_vec(), le("0dfbaa"));
    let rpa = [0xaa, 0xfb, 0x0d, 0x94, 0x81, 0x70];
    assert!(resolves(&irk, &rpa));
    assert!(!resolves(&[0u8; 16], &rpa));
    // Addresses whose top bits are not 0b01 are not resolvable private addresses.
    assert!(!resolves(&irk, &[0xaa, 0xfb, 0x0d, 0x94, 0x81, 0xB0]));
    let mut store = BondStore::default();
    store.upsert(Bond {
        address: ksbt::Address::new(0, [9; 6]),
        ltk: [1; 16],
        authenticated: false,
        irk: Some(irk),
        kind: 1,
    });
    assert!(store
        .find_for_advertiser(&ksbt::Address::new(1, rpa))
        .is_some());
    assert!(store
        .find_for_advertiser(&ksbt::Address::new(1, [0xaa, 0xfb, 0x0d, 0x94, 0x81, 0x71]))
        .is_none());
}

#[test]
fn l2cap_reassembly_handles_fragments_and_garbage() {
    use ksbt::l2cap::{fragment, Reassembler};
    let payload: Vec<u8> = (0..100u8).collect();
    let pkts = fragment(0x40, 4, &payload, 27);
    assert!(pkts.len() > 3);
    let mut r = Reassembler::default();
    let mut out = None;
    for p in &pkts {
        let (h, d) = hci::parse_acl(p).unwrap();
        out = r.push(&h, d).or(out);
    }
    assert_eq!(out.unwrap().payload, payload);
    // A continuation without a start, and an oversize announcement, are dropped.
    let (h, d) = hci::parse_acl(&pkts[1]).unwrap();
    assert!(Reassembler::default().push(&h, d).is_none());
    let huge = [0xFF, 0xFF, 4, 0, 1, 2];
    let (h, _) = hci::parse_acl(&pkts[0]).unwrap();
    assert!(Reassembler::default().push(&h, &huge).is_none());
}
