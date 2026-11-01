# ADR-0007: Pinned stable Rust toolchain

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Supersedes:** [ADR-0004](0004-rust-implementation-language.md) (toolchain channel only)
- **Stage/Phase:** S1

## Context
ADR-0004 assumed a pinned nightly. Bring-up showed nothing in Stage 1 needs
nightly: `x86_64-unknown-none` ships a prebuilt `core`, `asm!`, `extern "efiapi"`,
`#[link_section]` and `#[used]` are stable. The `limine` crate, however, requires
nightly (`#![feature(ptr_metadata)]`) and targets a newer protocol revision than
our pinned bootloader.

## Decision
- Pin **stable Rust 1.97.0** in `rust-toolchain.toml` (components: rustfmt,
  clippy, rust-src, llvm-tools; target `x86_64-unknown-none`).
- Write the Limine request/response bindings in-tree (`kernel/src/limine.rs`)
  against the protocol specification instead of depending on the crate.
- Everything else from ADR-0004 stands (Rust, `no_std`, minimal asm, documented `unsafe`).
- `#![deny(clippy::undocumented_unsafe_blocks)]` enforces the `// SAFETY:` rule.

## Alternatives considered
- Nightly + `limine` crate: less code, but unstable toolchain and a dependency
  whose revision support did not match our bootloader pin.

## Consequences
Reproducible, supported toolchain; ~200 lines of protocol bindings to maintain and
review at each Limine upgrade. Features that truly need nightly (e.g. `build-std`,
Miri on the kernel crate) require a new ADR.
