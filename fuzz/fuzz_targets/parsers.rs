#![no_main]
//! Coverage-guided fuzzing of every firmware/device parser in `ksparse`.
//! Run with: `cd fuzz && cargo +nightly fuzz run parsers`

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ksparse::harness::exercise(data);
});
