# Fuzzing the firmware/device parsers

`ksparse` parses data from untrusted sources (ACPI/UEFI tables, PCI configuration space).
The same entry point (`ksparse::harness::exercise`) is used by:

- `cargo test -p ksparse` — deterministic mutation / noise tests, run on every build and in CI;
- `cargo +nightly fuzz run parsers` (from this directory) — coverage-guided libFuzzer runs.

The contract: **no input may panic, hang or read out of bounds.** Any crash found by the fuzzer is
a security bug; add its input as a regression test in `crates/ksparse/tests/`.
