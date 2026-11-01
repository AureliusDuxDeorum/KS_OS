# ADR-0004: Rust (nightly-pinned) with minimal assembly

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S1

## Context
A kernel's dominant vulnerability class is memory-safety errors.

## Decision
The kernel and userspace services are written in Rust (`no_std`, target
`x86_64-unknown-none`). Assembly is limited to entry stubs, context switch, and
syscall/interrupt trampolines. The toolchain is pinned via `rust-toolchain.toml`
for reproducibility. `unsafe` is confined to small, documented modules and
requires `// SAFETY:` comments (see CONTRIBUTING).

## Alternatives considered
- C: no safety net. Zig: smaller ecosystem and tooling for verification.

## Consequences
Pinned nightly may need periodic, ADR-noted upgrades; Miri/`cargo-fuzz` become available.
