# ADR-0027: Licensing — Apache-2.0

- **Status:** Accepted
- **Date:** 2026-11-02
- **Deciders:** Project owner
- **Resolves:** the licensing question of [ADR-0022](0022-project-vision-and-end-goal.md)

## Context
The owner chose between Apache-2.0 and MPL-2.0 for a currently private project, with the requirement
to stay on the safe side of German law (`docs/legal/GERMAN-LAW-NOTES.md`).

## Decision
- All code and documentation in this repository is licensed under the **Apache License 2.0**
  (`LICENSE`, canonical text; copyright notice in `NOTICE`; `SPDX-License-Identifier: Apache-2.0` in
  every source file; `license = "Apache-2.0"` in the Cargo manifests).
- **Names and logos are not licensed** (Apache-2.0 §6): `TRADEMARKS.md`. Branding assets get their own
  folder and terms when they arrive (Phase 8).
- **Contributions** are accepted under the same licence (inbound = outbound, Apache-2.0 §5) with a
  Developer Certificate of Origin sign-off (`Signed-off-by:` on each commit). German copyright cannot be
  assigned (§ 29 UrhG), so a CLA would only ever be a licence grant; the DCO plus §5 is sufficient and lighter.
- **Third-party notices** travel with every binary release: `cargo xtask release` generates
  `THIRD-PARTY-LICENSES.txt` from the exact dependency graph of the kernel image (plus the bootloader),
  and ships `LICENSE`, `NOTICE` and that file in the release folder.
- Firmware blobs are never redistributed (ADR-0013/0018).

## Consequences
Anyone receiving a build may use, modify and redistribute it, including in closed-source products; the
patent grant and the trademark reservation are the protections that remain. Switching later to a
stricter licence (e.g. MPL-2.0) would need the consent of every contributor, so this decision should be
revisited *before* accepting outside contributions if protection against closed forks becomes important.
GPL-licensed code (e.g. ported Linux drivers) must stay in separate components.
