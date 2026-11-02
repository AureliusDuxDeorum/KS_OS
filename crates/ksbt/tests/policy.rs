// SPDX-License-Identifier: Apache-2.0
//! Pairing/connection policy: strangers only inside the pairing window, bonded first, bounded attempts, cooldown.
use ksbt::policy::{Candidate, Policy, COOLDOWN_MS, MAX_ATTEMPTS_PER_ROUND};
use ksbt::Address;

fn addr(n: u8) -> Address {
    Address {
        kind: 1,
        bytes: [n, 0, 0, 0, 0, 0],
    }
}

fn cand(n: u8, bonded: bool, pairable_hid: bool) -> Candidate {
    Candidate {
        address: addr(n),
        bonded,
        pairable_hid,
        already_connected: false,
    }
}

#[test]
fn strangers_are_ignored_unless_the_window_is_open() {
    let mut p = Policy::new();
    let c = [cand(1, false, true)];
    assert!(
        p.plan(0, false, &c).is_empty(),
        "no window: a fake keyboard must not be paired"
    );
    assert_eq!(p.plan(0, true, &c), vec![0]);
}

#[test]
fn only_hid_devices_in_pairing_mode_qualify_even_in_the_window() {
    let mut p = Policy::new();
    assert!(p.plan(0, true, &[cand(1, false, false)]).is_empty());
}

#[test]
fn bonded_devices_reconnect_without_a_window_and_go_first() {
    let mut p = Policy::new();
    let c = [
        cand(1, false, true),
        cand(2, true, false),
        cand(3, false, true),
        cand(4, true, false),
    ];
    assert_eq!(p.plan(0, false, &c), vec![1, 3]);
    assert_eq!(
        p.plan(0, true, &c),
        vec![1, 3, 0],
        "bonded first, then strangers, capped"
    );
}

#[test]
fn attempts_per_round_are_bounded_even_under_an_advertising_flood() {
    let mut p = Policy::new();
    let flood: Vec<_> = (0..200u8).map(|n| cand(n, false, true)).collect();
    assert_eq!(p.plan(0, true, &flood).len(), MAX_ATTEMPTS_PER_ROUND);
    // A known keyboard still gets its turn at the front of a flood.
    let mut flood = flood;
    flood.push(cand(250, true, false));
    assert_eq!(p.plan(0, true, &flood)[0], 200);
}

#[test]
fn failed_addresses_cool_down_and_recover() {
    let mut p = Policy::new();
    let c = [cand(1, true, false)];
    p.failed(addr(1), 1_000);
    assert!(p.plan(1_001, true, &c).is_empty());
    assert!(p.plan(1_000 + COOLDOWN_MS - 1, true, &c).is_empty());
    assert_eq!(p.plan(1_000 + COOLDOWN_MS, true, &c), vec![0]);
}

#[test]
fn connected_devices_are_not_retried_and_cooldown_memory_is_bounded() {
    let mut p = Policy::new();
    let mut c = cand(1, true, false);
    c.already_connected = true;
    assert!(p.plan(0, true, &[c]).is_empty());
    for n in 0..=255u8 {
        p.failed(addr(n), 0);
    }
    // Oldest entries were evicted; the newest are still cooling down.
    assert!(p.cooling_down(&addr(255), 1));
    assert!(!p.cooling_down(&addr(0), 1));
}
