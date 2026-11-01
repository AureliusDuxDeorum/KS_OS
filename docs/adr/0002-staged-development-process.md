# ADR-0002: Staged development process with hardening gates

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** cross-cutting

## Context
OS construction is large and security-sensitive. We need a predictable cadence
that keeps security from being deferred.

## Decision
Work is structured as **Stages → Phases → Steps**.

- **Step:** the smallest unit; maps to one or a few clean commits.
- **Phase:** a coherent set of steps with a verifiable exit criterion.
- **Stage:** a set of phases delivering a demonstrable capability.
- **Hardening gate:** between *every* pair of stages (and after the final one) a
  dedicated gate runs before the next stage begins. Minimum content:
  1. Threat-model update for new attack surface.
  2. `unsafe` inventory and audit (each block justified, minimised).
  3. Static analysis (clippy pedantic, `cargo-deny`, `cargo-audit`) clean.
  4. Fuzzing/Miri/sanitizer runs where applicable to new code.
  5. Mitigation review (W^X, stack protection, guard pages, KASLR/SMEP/SMAP/UMIP
     enablement as the platform allows).
  6. Regression boot tests green in CI.
  Gate results are recorded in `docs/hardening/stage-N.md`; a stage is not
  closed until its gate passes or a waiver ADR is accepted.

IDs: `S<stage>.P<phase>.<step>`, tracked in `docs/ROADMAP.md`.

## Alternatives considered
- Continuous security only: no forcing function, tends to slip.
- One hardening pass at the end: late discovery of design-level flaws.

## Consequences
Slower raw feature throughput; earlier detection of structural security issues.
