// SPDX-License-Identifier: Apache-2.0
#![no_main]
//! Coverage-guided fuzzing of the TPM client: hostile TPM responses and damaged stored records.
//! Run with: `cd fuzz && cargo +nightly fuzz run tpm`

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    kstpm::harness::exercise(data);
});
