# Contributing

Applies equally to the primary maintainer (Claude) and to any other party.

## Licence and sign-off
- The project is licensed under the Apache License 2.0 (`LICENSE`, ADR-0027). By contributing you agree your
  contribution is licensed the same way (Apache-2.0 §5).
- Every commit from an outside contributor carries a **Developer Certificate of Origin** sign-off (`git commit -s`, i.e. a
  `Signed-off-by: Your Name <you@example.com>` line), certifying that you wrote the change or have the right to
  submit it under this licence (https://developercertificate.org).
  (Commits made by the AI maintainer on the owner's behalf do not carry a sign-off: an AI cannot certify origin, so the
  owner is responsible for them.)
- New source files start with `// SPDX-License-Identifier: Apache-2.0`.
- Do not copy code from GPL/AGPL projects into this repository; code of unknown origin is not accepted.
- Do not add firmware blobs or branding assets without an explicit licence review.

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
