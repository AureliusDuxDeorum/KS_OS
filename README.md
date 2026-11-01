# KS_OS

A custom capability-based microkernel operating system for x86_64, written in Rust
(plus minimal assembly). This repository is the construction site: source, build
tooling, and the decision record.

**Current state (Stage 1):** boots under UEFI via Limine, announces **OS operating**
on serial and screen, and offers a **Power off** button and a **Boot into UEFI**
button (keyboard or PS/2 mouse).

## Quick start

Requirements: Rust via `rustup` (the pinned toolchain installs itself), `git`,
`qemu-system-x86`, `ovmf`.

```bash
cargo xtask run          # build, create image, boot in QEMU (serial on this terminal)
cargo xtask test         # end-to-end tests: boot banner, power off (key + mouse), UEFI setup
cargo xtask image        # just build target/ks_os.img (write it to a USB stick for hardware)
```

In the QEMU window: **Tab / arrows + Enter**, **P** (power off), **U** (UEFI setup),
or click the buttons.

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
