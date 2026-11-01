# ADR-0026: Release process and versioning (X.Y.Z)

- **Status:** Accepted
- **Date:** 2026-11-02
- **Deciders:** Project owner, Claude
- **Stage/Phase:** cross-cutting

## Context
The owner downloads boot images to test on real hardware and wants one obvious, clearly labelled
place to find them, and a version scheme with a fixed meaning.

## Decision
- **Scheme `X.Y.Z`:** `X` = finished (`0` until the ADR-0022 end goal is reached, then `1`), `Y` =
  version (feature release), `Z` = bug patch. Defined in `docs/VERSIONING.md`.
- **One source of truth:** `[workspace.package] version` in the root `Cargo.toml`; the kernel, the
  boot screen, the release folder name and the tests all read it.
- **`cargo xtask release`** produces `KS_OS-vX.Y.Z/` at the repository root containing the boot image,
  `README.txt` (plain text, how to write and boot it), `SHA256SUMS`, `LICENSES.txt` and
  `add-bluetooth-firmware.sh`. The release image is always an optimised build with **no firmware and no
  kernel command line**; firmware cannot be redistributed, so the script adds it on the user's machine
  (verified by booting the modified image and observing the module in the kernel).
- Release folders are committed and kept, so every released version stays downloadable. The image is
  32 MiB (mostly zeros, so it compresses to a small fraction in git).
- The boot partition is 32 MiB: ample for the kernel plus firmware blobs.

## Consequences
Licensing of KS_OS itself remains an open owner decision (ADR-0022); `LICENSES.txt` covers only
third-party components until then. Tags (`vX.Y.Z`) are created by the owner when a release is accepted.
