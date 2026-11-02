// SPDX-License-Identifier: Apache-2.0
#![no_main]
//! Coverage-guided fuzzing of the Bluetooth host stack: HCI/L2CAP/ATT parsers, the MediaTek patch
//! parser, bond storage, and the GATT/HOGP/SMP client state machines against a hostile peer.
//! Run with: `cd fuzz && cargo +nightly fuzz run bluetooth`

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ksbt::harness::exercise(data);
});
