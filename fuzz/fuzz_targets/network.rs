// SPDX-License-Identifier: Apache-2.0
#![no_main]
//! Coverage-guided fuzzing of the networking code: NIC descriptor rings and virtqueues against a
//! hostile device, and the whole IP stack against hostile Ethernet frames.
//! Run with: `cd fuzz && cargo +nightly fuzz run network`

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ksnet::harness::exercise(data);
});
