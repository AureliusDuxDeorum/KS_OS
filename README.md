# KS_OS

A custom capability-based microkernel operating system for x86_64, written in Rust
(plus minimal assembly). This repository is the construction site: source, build
tooling, and the decision record.

**Current state (Stage 2, in progress):** boots under UEFI via Limine, announces **OS operating**
on serial and screen, and offers a **Power off** button and a **Boot into UEFI**
button (keyboard or mouse over PS/2, USB or Bluetooth LE), and brings up wired networking
(DHCP, DNS, ICMP, TCP). Coding agents / new contributors: start with [STRUCTURE.md](STRUCTURE.md).

## Download the boot image
The latest ready-to-write image is in the top-level folder **[`KS_OS-v0.3.0/`](KS_OS-v0.3.0/)** — one file to
write to a USB stick, plus a plain-text `README.txt`. Versions are `X.Y.Z` (see
[docs/VERSIONING.md](docs/VERSIONING.md)); changes are listed in [CHANGELOG.md](CHANGELOG.md).

## Quick start (developers)

Requirements: Rust via `rustup` (the pinned toolchain installs itself), `git`,
`qemu-system-x86`, `ovmf`.

```bash
cargo xtask run          # build, create image, boot in QEMU (serial on this terminal)
cargo xtask test         # end-to-end tests: boot banner, power off (key + mouse), UEFI setup
cargo xtask image        # just build target/ks_os.img
cargo xtask release      # assemble the KS_OS-vX.Y.Z/ release folder at the repository root
```

In the QEMU window (or with a USB or PS/2 keyboard and mouse on real hardware): **Tab / arrows +
Enter**, **P** (power off), **U** (UEFI setup), **L** (on-screen kernel log), or click the buttons.

## Where things live

| Path | Purpose |
| --- | --- |
| `kernel/` | The kernel (`no_std`, `x86_64-unknown-none`) |
| `xtask/` | Build / image / QEMU / test automation (`cargo xtask …`) |
| `docs/adr/` | Architecture Decision Records — the source of truth for *why* |
| `docs/ROADMAP.md` | Stages → phases → steps, with status |
| `docs/hardening/` | Security hardening gate records, one per stage |
| `CONTRIBUTING.md` | Commit rules and how outside changes are integrated |

## Development model

Work is organised as **Stages → Phases → Steps**. A **security hardening gate**
sits between every pair of stages. See
[ADR-0002](docs/adr/0002-staged-development-process.md).

## Licence

KS_OS is licensed under the [Apache License 2.0](LICENSE) (see [NOTICE](NOTICE) and
[ADR-0027](docs/adr/0027-licensing.md)). The KS name and logos are not covered by that licence
([TRADEMARKS.md](TRADEMARKS.md)). Third-party components are listed in the release's
`THIRD-PARTY-LICENSES.txt`. Security reports: [SECURITY.md](SECURITY.md).
