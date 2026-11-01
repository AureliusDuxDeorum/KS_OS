# Contributing

Applies equally to the primary maintainer (Claude) and to any other party.

## Commits
- One logical change per commit; the tree builds and tests pass at every commit.
- [Conventional Commits](https://www.conventionalcommits.org/): `type(scope): summary`
  (`feat`, `fix`, `docs`, `refactor`, `test`, `build`, `ci`, `chore`, `security`).
- Imperative summary, ≤ 72 chars; body explains *why*.
- Reference the stage/phase/step and ADR where relevant, e.g. `Refs: S1.P2.3, ADR-0005`.

## ADRs
- Any change to architecture, toolchain, process, or an invariant needs an ADR.
- ADRs are immutable once `Accepted`; change course with a new ADR that
  `Supersedes` the old one and update the old one's status line only.
- Update `docs/adr/README.md` index and `docs/ROADMAP.md` in the same PR.

## Changes by other parties
- Work on a branch; do not push to another party's branch.
- Before starting on a stage, read the ROADMAP and ADR index. If your change
  conflicts with an Accepted ADR, open a new ADR (status `Proposed`) first.
- The maintainer reconciles ADR/ROADMAP drift when integrating and records it.

## unsafe Rust
Every `unsafe` block needs a `// SAFETY:` comment stating the invariant relied on.
