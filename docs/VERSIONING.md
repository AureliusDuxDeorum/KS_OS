# Versioning

KS_OS versions are written **X.Y.Z** and tagged `vX.Y.Z`.

| Part | Meaning | Rule |
| --- | --- | --- |
| **X** | finished or not | `0` = not finished. `1` = the end goal of [ADR-0022](adr/0022-project-vision-and-end-goal.md) has been reached. **It stays `0` until the owner declares the project finished.** |
| **Y** | version | Incremented for each release that adds or changes functionality. Resets `Z` to `0`. |
| **Z** | bug patch | Incremented for a release that only fixes bugs. |

Examples: `0.1.0` first release · `0.1.1` bug fixes to it · `0.2.0` next feature release · `1.0.0` finished.

## Where the number lives
One place: `version` under `[workspace.package]` in the root `Cargo.toml`. Every crate inherits it, the
kernel prints it at boot (`KS_OS 0.1.0 booting`, and on screen), and `cargo xtask release` names the
release folder and image from it. Change it there and nowhere else.

## Making a release
1. Bump the version in the root `Cargo.toml` and add an entry to `CHANGELOG.md`.
2. `cargo xtask test` must pass.
3. `cargo xtask release` creates `KS_OS-vX.Y.Z/` at the repository root: the boot image,
   `README.txt`, `SHA256SUMS`, `LICENSE`, `NOTICE`, `THIRD-PARTY-LICENSES.txt` and the Bluetooth-firmware helper. Commit that folder.
   Release images never contain firmware blobs.
4. Older release folders stay in the repository so earlier versions remain downloadable.
