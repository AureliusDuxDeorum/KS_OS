// SPDX-License-Identifier: Apache-2.0
//! Connection and pairing policy: who the host may talk to, in which order, and how often.
//!
//! Pure so that the rules that stand between the radio and the keyboard input path are unit-tested
//! (ADR-0030 S-01, S-02): unknown devices are only paired inside an explicit pairing window,
//! bonded devices are always preferred, a round makes a bounded number of attempts, and an address
//! that just failed is left alone for a while.

use crate::Address;
use alloc::vec::Vec;

pub const MAX_ATTEMPTS_PER_ROUND: usize = 3;
pub const COOLDOWN_MS: u64 = 60_000;
/// More cooldown entries than this are never needed; oldest are dropped (bounded memory).
const MAX_COOLDOWNS: usize = 64;

/// What the scan saw, reduced to what the policy needs.
#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub address: Address,
    /// We hold a bond for this device.
    pub bonded: bool,
    /// It advertises HID and is in general-discoverable (pairing) mode.
    pub pairable_hid: bool,
    pub already_connected: bool,
}

#[derive(Default)]
pub struct Policy {
    cooldown: Vec<(Address, u64)>,
}

impl Policy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a failed attempt; the address is skipped until `now + COOLDOWN_MS`.
    pub fn failed(&mut self, address: Address, now: u64) {
        self.cooldown.retain(|(a, _)| *a != address);
        if self.cooldown.len() >= MAX_COOLDOWNS {
            self.cooldown.remove(0);
        }
        self.cooldown
            .push((address, now.saturating_add(COOLDOWN_MS)));
    }

    pub fn cooling_down(&self, address: &Address, now: u64) -> bool {
        self.cooldown
            .iter()
            .any(|(a, until)| a == address && *until > now)
    }

    /// Indices into `candidates` to try this round, in order.
    pub fn plan(&mut self, now: u64, window_open: bool, candidates: &[Candidate]) -> Vec<usize> {
        self.cooldown.retain(|(_, until)| *until > now);
        let mut order: Vec<usize> = (0..candidates.len())
            .filter(|&i| {
                let Some(c) = candidates.get(i) else {
                    return false;
                };
                !c.already_connected
                    && !self.cooling_down(&c.address, now)
                    && (c.bonded || (window_open && c.pairable_hid))
            })
            .collect();
        // Bonded devices first, otherwise scan order; stable so results are deterministic.
        order.sort_by_key(|&i| candidates.get(i).is_none_or(|c| !c.bonded));
        order.truncate(MAX_ATTEMPTS_PER_ROUND);
        order
    }
}
