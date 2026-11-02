# KS_OS — structure and platform reference

> **For humans and coding agents.** Hand this single file to any assistant before it touches the code.
> It states what the OS is, what it provides, what it does **not** provide yet, the conventions that are
> enforced, and where everything lives. Sections marked *(generated)* are rewritten by
> `cargo xtask structure`; CI fails if this file is stale (`cargo xtask structure --check`).
> Hand-written text lives in `xtask/structure/STRUCTURE.md.in` — edit that, not this file.

- **Version:** `0.3.0` (`X.Y.Z`: X = finished (0/1), Y = version, Z = bug patch — `docs/VERSIONING.md`)
- **Repository:** `AureliusDuxDeorum/KS_OS` · **Licence:** Apache-2.0 (the KS name and logos are not licensed, `TRADEMARKS.md`)
- **Owner:** Nils Seidenberg · **Process:** Stages → Phases → Steps, a hardening gate between stages, every
  decision recorded as an ADR in `docs/adr/` · Conventional Commits, one logical change per commit.

## 1. What KS_OS is

A private, from-scratch, **capability-based microkernel OS** for **x86_64 + UEFI**, written in **Rust**
(plus minimal inline assembly). **Long-term goal ([ADR-0022](docs/adr/0022-project-vision-and-end-goal.md)):**
a fully capable desktop OS — Ubuntu-level polish, a monochrome "cybercore" design based on KS branding
(design rundown to be supplied by the owner at the UI phase), Arch-level openness — able to run demanding
developer applications such as coding agents (Claude Code). The project stays open until that is reached.

**Today (v0.x) it is a kernel only.** There is no user mode, no scheduler, no filesystem and no libc yet.
Everything runs in ring 0 in one address space, driven by a single boot CPU and a polled idle loop.

## 2. Platform facts an agent must not guess

| Topic | Fact |
|---|---|
| Architecture | x86_64 only, long mode, 4-level paging (no 5-level), single CPU in use (APs not started) |
| Firmware / boot | UEFI only. **Limine v11** (pinned commit, SHA-256 verified), boot protocol **base revision 5**; Limine bindings are hand-written (`kernel/src/limine.rs`) because the `limine` crate needs nightly |
| Image | GPT disk, 32 MiB FAT ESP built by `cargo xtask image`; `\EFI\BOOT\BOOTX64.EFI`, `limine.conf`, `kernel.elf`, optional firmware blobs as Limine modules |
| Toolchain | Rust **stable 1.97.0**, pinned in `rust-toolchain.toml`; target `x86_64-unknown-none`; `#![no_std]` + `alloc`; no nightly features in the main build (fuzzing under `fuzz/` uses nightly separately) |
| Linking | Static, non-PIE, higher half at `0xffffffff80000000`, W^X segments (`kernel/linker.ld`, `.cargo/config.toml`) |
| Memory | Limine HHDM gives direct map of RAM (`BootInfo::hhdm`); bitmap frame allocator (`pmm`, supports contiguous DMA runs); kernel-owned page tables (`vmm`/`paging`) with strict **W^X** and PAT (write-combining framebuffer, uncached MMIO window in PML4 slot reserved for `vmm::map_mmio`); slab heap (`heap`) behind `alloc` |
| Interrupts | Own GDT/TSS/IDT, IST stacks for #DF/NMI/#MC, guard-paged stacks; xAPIC (x2APIC opt-in `apic=x2`, untested), IOAPIC via MADT, MSI/MSI-X, 1 kHz tick, TSC calibrated from the ACPI PM timer |
| Concurrency | No threads or scheduler. `sync::IrqMutex` (spin + IRQ disable) protects state shared with interrupt handlers. Drivers are **polled from the UI idle loop** (`ui.rs`) |
| Firmware services | UEFI runtime services relocated with `SetVirtualAddressMap` (Memory Attributes Table W^X policy); ACPI S5 power-off; reboot-to-firmware-setup via `OsIndications` |
| Input | PS/2 (interrupt-driven), USB HID via xHCI (hubs, full report-descriptor parser), Bluetooth LE HID-over-GATT (new devices only pair inside a pairing window: auto for 120 s if no other input exists, else key `B`); all feed `input::Event` queue |
| Network | Intel igc (I225/I226) + igb (QEMU), virtio-net; smoltcp stack: DHCPv4, DNS (A), ICMP echo, TCP client/listen; IPv4 only; **no UDP/TCP socket API for applications yet** (kernel-internal `net::with`) |
| Bluetooth | USB controllers; MediaTek MT7921 bring-up (experimental), HCI/L2CAP/ATT/GATT/SMP (LE Secure Connections only)/HOGP; Classic BR/EDR **not** implemented; bonds in UEFI NV variable `KSOS-BT-BONDS` (unencrypted — known gap) |
| Graphics | UEFI GOP linear framebuffer only (`fb.rs`), 8x8 bitmap font, monochrome placeholder UI. No GPU driver, no compositor |
| Storage | Only USB bulk-only mass storage *transport test*. No block layer, filesystem or persistent storage yet |
| Randomness | `rng`: RDSEED/RDRAND only for keys (no software fallback); `rng::network_seed` may fall back to TSC mixing for non-cryptographic use |
| Security gaps (tracked) | IOMMU not enabled (DMA unrestricted); Bluetooth bonds unencrypted; no user/kernel separation yet — see `docs/hardening/` |

## 3. What is missing before demanding apps (e.g. Claude Code) can run

In dependency order (see `docs/ROADMAP.md` for the authoritative plan):

1. Threads, scheduler, ring-3 user mode, syscalls, ELF loader (Stage 3).
2. Capabilities + IPC, driver isolation / service model (Stage 4) — the microkernel part.
3. Block layer, VFS, a real filesystem, persistent storage (Stage 5).
4. A libc/POSIX layer (or a native runtime) plus a toolchain, so existing runtimes (Node/Bun/Rust binaries) can be ported (Stage 7).
5. A **socket API for userland**, TLS root store, system clock/RTC and time zones, entropy source for userland (`getrandom`), `/dev/tty`-style terminal, pipes, process spawning, signals.
6. Graphics/compositor and fonts for a GUI terminal (Stage 6); Wi-Fi (Stage 8).

## 4. Conventions that are enforced (CI + review)

- **Licence header:** every source file starts with `// SPDX-License-Identifier: Apache-2.0` (`#` for scripts).
- **`unsafe`:** only in `kernel/` (and `xtask` never). Every `unsafe` block carries a `// SAFETY:` comment.
  Library crates (`ksparse`, `ksbt`, `ksnet`) are `#![forbid(unsafe_code)]`, `no_std`, pure and host-testable.
- **Untrusted input** (ACPI, PCI, USB descriptors, HID, Bluetooth, network frames, device-written rings) is
  parsed only by bounded code in the library crates, with host tests **and** a libFuzzer target in `fuzz/`.
  Never trust fields in memory shared with a device (copy link/length fields into driver-private state).
- **Logging:** `kprintln!` (serial + `klog` ring, viewable on screen with the `L` key). Hardware bring-up code
  logs generously, with a short subsystem prefix (`usb:`, `bt:`, `net:` …).
- **Hardware-agnostic logic goes into a library crate; only MMIO/DMA/PCI glue stays in `kernel/`.**
  Drivers are written against traits (`ksnet::Nic`, `ksnet::dma::DmaMem`, `ksbt::Transport`) so they can be
  simulated on the host.
- **Tests:** `cargo test --workspace --exclude kernel` (host), `cargo xtask test` (QEMU + OVMF end-to-end,
  `KS_TEST=<substring>` selects tests), `cd fuzz && cargo +nightly fuzz run <parsers|bluetooth|network>`.
  A new driver is not done without a QEMU-observable test or an explicit "hardware-unverified" note in
  `docs/hardware-testing.md`.
- **Quality gates:** `cargo fmt --all --check`, `cargo clippy --workspace -- -D warnings`, `cargo deny check`
  (licences: MIT, Apache-2.0, BSD-3-Clause, 0BSD only), `cargo xtask structure --check`.
- **Security reviews** run at every stage gate (scan → record in an ADR → fix); the latest is `docs/adr/0030-security-review-1.md` and its open items (IOMMU, unencrypted bonds, unauthenticated boot chain) block Gate G2.
  The release profile has `overflow-checks = true`; the kernel `deny`s `undocumented_unsafe_blocks`.
- **Decisions** go into an ADR (`docs/adr/template.md`); never silently contradict one — supersede it.
- **Releases:** `cargo xtask release` regenerates `KS_OS-vX.Y.Z/` (image, README, SHA256SUMS, licences). Tags are the owner's.

## 5. Commands

```bash
cargo xtask build                 # kernel ELF
cargo xtask image                 # bootable GPT/ESP disk image
cargo xtask run                   # boot in QEMU (needs OVMF); QEMU_EXTRA="..." for more flags
cargo xtask test                  # full end-to-end suite (KS_TEST=net for a subset)
cargo xtask release               # rebuild the release folder
cargo xtask structure [--check]   # regenerate / verify this file
cargo test --workspace --exclude kernel
```

Kernel command line (Limine `limine.conf`, set by xtask): `selftest=<name>` runs a self-test instead of the
UI; `apic=x2` enables x2APIC; `nettest.port=N nettest.dns=NAME` parameterise the `net` self-test.

## 6. How to add a driver (recipe)

1. Put register/descriptor/state-machine logic in a library crate with a host test against a simulated device.
2. Add the kernel glue under `kernel/src/<subsystem>/`: PCI match (`pci::devices()`), `pci::map_bar`,
   `dma::Dma`, and call `<subsystem>::init(&boot)` from `kernel/src/main.rs` after `usb::init`.
3. Poll it from `ui.rs`'s idle loop (`<subsystem>::service()`); expose status through a `take_status()` line.
4. Add a `selftest=<name>` case in `kernel/src/selftest.rs` and an end-to-end test in `xtask/src/tests.rs`.
5. Write/extend an ADR; update `docs/ROADMAP.md`, `CHANGELOG.md` and re-run `cargo xtask structure`.

## 7. Crates *(generated)*

| Crate | Path | Role |
|---|---|---|
| `ksbt` | `crates/ksbt/` | KS_OS Bluetooth host stack: HCI, L2CAP, ATT/GATT, SMP (LE Secure Connections) and HID-over-GATT as pure, host-testable code. |
| `ksnet` | `crates/ksnet/` | KS_OS networking: NIC abstraction, igc/virtio descriptor logic and the smoltcp-based IP stack (DHCP, DNS, ICMP, TCP) as pure, host-testable code. |
| `ksparse` | `crates/ksparse/` | Pure, bounds-checked parsers for firmware and hardware data (ACPI, PCI) shared by the KS_OS kernel and its host-side tests/fuzzers. |
| `ksparse-fuzz` | `fuzz/` | libFuzzer targets (nightly; standalone workspace). |
| `kernel` | `kernel/` | The kernel binary (`no_std`, ring 0). All `unsafe` hardware glue lives here. |
| `xtask` | `xtask/` | Host tooling: build, image, QEMU tests, release, this document (`cargo xtask …`). |

## 8. Repository tree *(generated)*

Each entry shows the first documentation line of the file.

```text
KS_OS/
├── .cargo/
│   └── config.toml — Cargo target flags (static, non-PIE)
├── .github/
│   └── workflows/
│       └── ci.yml — GitHub Actions: build, lint, test, deny, structure check
├── .gitignore — Ignored files
├── CHANGELOG.md — Changelog
├── CONTRIBUTING.md — Contributing
├── Cargo.lock — Pinned dependency versions
├── Cargo.toml — Workspace manifest: members and the single release version
├── KS_OS-v0.1.0/ — release: boot image, SHA256SUMS, licences (7 files)
├── KS_OS-v0.2.0/ — release: boot image, SHA256SUMS, licences (7 files)
├── KS_OS-v0.2.1/ — release: boot image, SHA256SUMS, licences (7 files)
├── KS_OS-v0.3.0/ — release: boot image, SHA256SUMS, licences (7 files)
├── LICENSE — Apache License 2.0
├── NOTICE — Copyright and third-party notice
├── README.md — KS_OS
├── SECURITY.md — Security policy
├── STRUCTURE.md — This file: platform and structure reference for coding agents (generated)
├── TRADEMARKS.md — Names and logos
├── crates/
│   ├── ksbt/
│   │   ├── Cargo.toml — KS_OS Bluetooth host stack: HCI, L2CAP, ATT/GATT, SMP (LE Secure Connections) and HID-over-GATT as pure,…
│   │   ├── src/
│   │   │   ├── att.rs — ATT (Core Spec Vol 3, Part F): PDU opcodes, error codes, UUIDs and PDU builders
│   │   │   ├── bond.rs — Bond storage format: what survives a reboot so that a paired device can reconnect and re-encrypt without…
│   │   │   ├── crypto.rs — Bluetooth LE security toolbox (Core Spec Vol 3, Part H, 2.2): AES-128, AES-CMAC, the LE Secure Connections…
│   │   │   ├── framing.rs — Re-assembly of HCI packets from USB endpoint transfers
│   │   │   ├── gatt.rs — GATT client procedures over an ATT link: service, characteristic and descriptor discovery, long reads and…
│   │   │   ├── harness.rs — Fuzz/robustness entry point: drives every parser and both client state machines with arbitrary bytes
│   │   │   ├── hci.rs — HCI (Core Spec Vol 4, Part E): command builders, event/ACL parsing, advertising data
│   │   │   ├── hogp.rs — HID over GATT Profile (HOGP) client: finds the HID service, reads the report map, maps report…
│   │   │   ├── host.rs — The Bluetooth host: drives a controller through a [`Transport`] and exposes connected peers as ATT and SMP…
│   │   │   ├── l2cap.rs — L2CAP basic frames over LE ACL links: reassembly, fragmentation and LE signalling
│   │   │   ├── lib.rs — Bluetooth host stack
│   │   │   ├── mtk.rs — MediaTek Bluetooth controller support: patch-file parsing and the WMT vendor protocol codec
│   │   │   ├── policy.rs — Connection and pairing policy: who the host may talk to, in which order, and how often
│   │   │   └── smp.rs — Security Manager Protocol (Core Spec Vol 3, Part H): LE Secure Connections pairing as the initiator
│   │   └── tests/
│   │       ├── common/
│   │       │   └── mod.rs — A simulated Bluetooth LE controller and HID keyboard peripheral, used to test the whole host stack
│   │       ├── crypto_vectors.rs — Cryptographic functions against the Bluetooth Core Specification sample data (Vol 3, Part H, Appendix D) and…
│   │       ├── hw_codecs.rs — USB packet framing and the (hardware-unverified) MediaTek codec, against synthetic data
│   │       ├── policy.rs — Pairing/connection policy: strangers only inside the pairing window, bonded first, bounded attempts, cooldown
│   │       └── stack.rs — End-to-end tests of the Bluetooth host stack against the simulated controller and keyboard
│   ├── ksnet/
│   │   ├── Cargo.toml — KS_OS networking: NIC abstraction, igc/virtio descriptor logic and the smoltcp-based IP stack (DHCP, DNS,…
│   │   ├── src/
│   │   │   ├── dma.rs — Access to device-shared memory (descriptor rings, packet buffers)
│   │   │   ├── harness.rs — Fuzz/robustness entry point: a hostile network and a hostile device
│   │   │   ├── igc.rs — Intel igc (I225/I226 2.5GbE) and igb (82576, the QEMU test model) definitions and ring logic
│   │   │   ├── lib.rs — KS_OS networking as pure, host-testable code
│   │   │   ├── nic.rs — The driver-facing NIC trait and its adapter into smoltcp's `Device`
│   │   │   ├── stack.rs — The IP stack: DHCPv4 client, DNS resolver, ICMP echo and TCP client sockets over smoltcp
│   │   │   └── virtio.rs — virtio-net over the modern (1.x) PCI transport: layout constants and split-virtqueue logic
│   │   └── tests/
│   │       ├── common/
│   │       │   └── mod.rs — Simulation helpers: DMA memory model, an Ethernet "wire", and a scripted DHCP/DNS server
│   │       ├── rings.rs — igc descriptor-ring and virtqueue logic against simulated devices that behave like the hardware
│   │       └── stack.rs — End-to-end tests of the IP stack (DHCP, ping, DNS, TCP with packet loss, link loss) against a scripted server
│   └── ksparse/
│       ├── Cargo.toml — Pure, bounds-checked parsers for firmware and hardware data (ACPI, PCI) shared by the KS_OS kernel and its…
│       ├── src/
│       │   ├── bytes.rs — Bounds-checked little-endian readers
│       │   ├── efi.rs — UEFI data structures handed over by firmware: memory-map descriptors, configuration tables and the Memory…
│       │   ├── harness.rs — Entry points shared by the host robustness tests and the fuzz targets
│       │   ├── hid.rs — HID report-descriptor parsing and input-report interpretation (HID 1.11)
│       │   ├── lib.rs — Pure, allocation-light parsers for untrusted firmware and hardware data
│       │   ├── madt.rs — ACPI MADT (multiple APIC description table) parser
│       │   ├── mcfg.rs — ACPI MCFG table: where PCI Express enhanced configuration (ECAM) space lives
│       │   ├── pci.rs — PCI configuration-space decoding: headers, capabilities, BARs, MSI/MSI-X
│       │   ├── sdt.rs — ACPI system description tables: RSDP, table headers, FADT and the `\_S5` object
│       │   └── usb.rs — USB descriptors and control-request helpers
│       └── tests/
│           ├── parsers.rs — Unit and conformance tests of the firmware/hardware parsers against hand-built tables
│           ├── robustness.rs — Randomised and exhaustive-mutation robustness tests: no input may panic or hang
│           └── support.rs — Builders for valid sample tables, shared by the integration tests
├── deny.toml — cargo-deny policy (licences, advisories, sources)
├── docs/
│   ├── ROADMAP.md — Roadmap
│   ├── VERSIONING.md — Versioning
│   ├── adr/
│   │   ├── 0001-record-architecture-decisions.md — ADR-0001: Record architecture decisions
│   │   ├── 0002-staged-development-process.md — ADR-0002: Staged development process with hardening gates
│   │   ├── 0003-target-architecture-x86_64.md — ADR-0003: Target architecture is x86_64
│   │   ├── 0004-rust-implementation-language.md — ADR-0004: Rust (nightly-pinned) with minimal assembly
│   │   ├── 0005-capability-based-microkernel.md — ADR-0005: Capability-based microkernel baseline
│   │   ├── 0006-boot-and-test-tooling.md — ADR-0006: Limine boot, QEMU testing, GitHub Actions CI
│   │   ├── 0007-stable-rust-toolchain.md — ADR-0007: Pinned stable Rust toolchain
│   │   ├── 0008-stage-1-scope.md — ADR-0008: Stage 1 scope — boot, announce, power off / boot to UEFI setup
│   │   ├── 0009-uefi-runtime-services.md — ADR-0009: UEFI Runtime Services via a virtual window and Memory Attributes Table
│   │   ├── 0010-power-off-path.md — ADR-0010: Power-off strategy
│   │   ├── 0011-ps2-polling-input.md — ADR-0011: PS/2 polling input for Stage 1
│   │   ├── 0012-limine-pin-and-base-revision.md — ADR-0012: Limine v11 pinned by commit and checksum; base revision 5
│   │   ├── 0013-stage-2-scope.md — ADR-0013: Stage 2 scope — Bluetooth input and wired LAN internet
│   │   ├── 0014-exception-handling-and-stacks.md — ADR-0014: CPU exception handling, IST stacks and guard-paged kernel stacks
│   │   ├── 0015-placeholder-monochrome-ui.md — ADR-0015: Placeholder monochrome UI
│   │   ├── 0016-target-hardware-inventory.md — ADR-0016: Target hardware inventory and input/Bluetooth priorities
│   │   ├── 0017-bluetooth-first-internal-controller.md — ADR-0017: Bluetooth is the primary input path; internal MediaTek controller first
│   │   ├── 0018-mediatek-mt7961-bluetooth-target.md — ADR-0018: MediaTek MT7961 (MT7921K) Bluetooth — concrete target and firmware handling
│   │   ├── 0019-ble-hid-first-and-validation-targets.md — ADR-0019: BLE HID first; validation targets from the owner's Linux logs
│   │   ├── 0020-memory-management.md — ADR-0020: Physical memory manager, kernel-owned address space and heap
│   │   ├── 0021-interrupts-and-timekeeping.md — ADR-0021: Interrupt architecture and timekeeping
│   │   ├── 0022-project-vision-and-end-goal.md — ADR-0022: Project vision and end goal
│   │   ├── 0023-acpi-pci-and-untrusted-parsers.md — ADR-0023: ACPI/PCI discovery, untrusted-data parsers and the AML decision
│   │   ├── 0024-usb-xhci-and-hid.md — ADR-0024: USB stack — xHCI driver, enumeration, hubs, HID, and the on-screen kernel log
│   │   ├── 0025-bluetooth-host-stack.md — ADR-0025: Bluetooth host stack — design, security decisions and verification boundary
│   │   ├── 0026-release-process-and-versioning.md — ADR-0026: Release process and versioning (X.Y.Z)
│   │   ├── 0027-licensing.md — ADR-0027: Licensing — Apache-2.0
│   │   ├── 0028-networking-stack.md — ADR-0028: Networking: smoltcp stack, igc/igb and virtio-net drivers
│   │   ├── 0029-structure-document-for-agents.md — ADR-0029: STRUCTURE.md — a generated platform reference for coding agents
│   │   ├── 0030-security-review-1.md — ADR-0030: Security review 1 — scan results and dispositions
│   │   ├── README.md — Architecture Decision Records
│   │   └── template.md — ADR-NNNN: Title
│   ├── hardening/
│   │   └── stage-1.md — Hardening gate G1 — end of Stage 1
│   ├── hardware-testing.md — Hardware test guide
│   └── legal/
│       └── GERMAN-LAW-NOTES.md — Notes on German and EU law for KS_OS
├── fuzz/
│   ├── Cargo.lock — Pinned dependency versions
│   ├── Cargo.toml
│   ├── README.md — Fuzzing the firmware/device parsers
│   └── fuzz_targets/
│       ├── bluetooth.rs
│       ├── network.rs
│       └── parsers.rs
├── kernel/
│   ├── Cargo.toml
│   ├── build.rs — Passes the kernel linker script to the linker
│   ├── linker.ld — Kernel linker script (higher half, W^X segments)
│   └── src/
│       ├── acpi.rs — ACPI: table discovery, PM timer and the S5 (soft-off) transition
│       ├── apic.rs — Local APIC driver (xAPIC MMIO or x2APIC MSR mode) for the boot CPU
│       ├── arch.rs — Thin, audited wrappers around privileged x86_64 instructions
│       ├── boot.rs — Safe view over the information the bootloader hands us
│       ├── bt/
│       │   ├── mediatek.rs — MediaTek MT79xx Bluetooth bring-up: identify the chip, pick and download the patch firmware through the WMT…
│       │   ├── mod.rs — Bluetooth: controller bring-up, scanning, pairing, bonded reconnection and HID input
│       │   └── transport.rs — HCI over USB: commands on the control endpoint, events on the interrupt IN endpoint, ACL data on the bulk…
│       ├── dma.rs — DMA-able memory for device rings, contexts and transfer buffers
│       ├── efi.rs — UEFI Runtime Services
│       ├── fb.rs — Linear-framebuffer renderer: rectangles, bitmap text and a software cursor
│       ├── frames.rs — Frame source handle used by page-table code; backed by the global [`crate::pmm`]
│       ├── gdt.rs — Global Descriptor Table and Task State Segment
│       ├── heap.rs — Kernel heap: size-class slab allocator over the physical memory manager
│       ├── idt.rs — Interrupt Descriptor Table and CPU exception handling
│       ├── input.rs — Unified input events and the queue USB/Bluetooth HID drivers push into
│       ├── ioapic.rs — I/O APIC driver: routes global system interrupts (GSIs) to vectors on the boot CPU
│       ├── irq.rs — Interrupt vector allocation and dispatch for vectors 32..=255
│       ├── klog.rs — In-memory kernel log: a ring buffer that mirrors everything written to the serial console, so the log can be…
│       ├── limine.rs — Minimal Limine boot protocol bindings (base revision 5)
│       ├── log.rs — Kernel logging macros
│       ├── main.rs — KS_OS kernel — Stage 1: boot, announce "OS operating", power off / boot to UEFI setup
│       ├── msi.rs — Message-signalled interrupt (MSI / MSI-X) message composition
│       ├── net/
│       │   ├── igc.rs — Intel I225/I226 (igc) driver; also drives the 82576 model that QEMU emulates as `igb` (CI)
│       │   ├── mod.rs — Networking: NIC drivers ([`igc`], [`virtio`]) feeding the `ksnet` IP stack
│       │   └── virtio.rs — virtio-net (modern PCI transport, polled)
│       ├── paging.rs — 4-level page-table construction and inspection
│       ├── pci.rs — PCI / PCI Express: configuration access, bus enumeration, BARs, MSI and MSI-X
│       ├── pmm.rs — Physical memory manager: a bitmap frame allocator
│       ├── ps2.rs — Polling driver for the legacy i8042 PS/2 controller (keyboard and mouse)
│       ├── rng.rs — Hardware random number generator (RDSEED, falling back to RDRAND)
│       ├── selftest.rs — Boot-time self-tests selected through the kernel command line (`selftest=<name>`), used by the end-to-end…
│       ├── serial.rs — 16550 UART driver (COM1), used as the primary debug console
│       ├── stacks.rs — Kernel-owned stacks, each with an unmapped guard page below it
│       ├── strbuf.rs — Fixed-capacity, allocation-free string buffer implementing `fmt::Write`
│       ├── sync.rs — Interrupt-safe spin lock
│       ├── time.rs — Timekeeping: calibrated TSC clock, local APIC timer tick and sleeping
│       ├── ui.rs — The Stage 1 screen: the "OS operating" banner and two buttons
│       ├── usb/
│       │   ├── hid.rs — USB HID class driver: keyboards and relative pointing devices feed the unified input queue
│       │   ├── hub.rs — USB hub class driver (high-speed and below; SuperSpeed hubs are reported as unsupported)
│       │   ├── mod.rs — USB core: device enumeration, the device registry and class-driver binding
│       │   └── xhci.rs — xHCI (USB 3.x) host controller driver
│       └── vmm.rs — Kernel virtual memory manager
├── rust-toolchain.toml — Pinned Rust toolchain
└── xtask/
    ├── Cargo.toml
    ├── release/
    │   ├── README.txt.in
    │   └── add-bluetooth-firmware.sh — Adds Bluetooth firmware files to a KS_OS boot image, in place
    ├── src/
    │   ├── disk.rs — Disk image construction: a FAT ESP wrapped in a GPT, built byte-for-byte reproducibly
    │   ├── firmware.rs — Firmware blobs (Bluetooth patch files and the like) supplied by the user at image-build time
    │   ├── limine.rs — Fetches the pinned, checksum-verified Limine UEFI bootloader
    │   ├── main.rs — Build, image, run and test automation for KS_OS (`cargo xtask <command>`)
    │   ├── notices.rs — Generates `THIRD-PARTY-LICENSES.txt`: the name, version, licence and full licence/notice texts of every…
    │   ├── qemu.rs — QEMU launch helpers
    │   ├── release.rs — `cargo xtask release`: assembles a clearly named, self-contained release folder at the repository root…
    │   ├── structure.rs — Generates `STRUCTURE.md`: the platform/structure reference that is handed to coding agents
    │   └── tests.rs — End-to-end tests: boot the real image in QEMU/OVMF and drive it
    └── structure/
        └── STRUCTURE.md.in — KS_OS — structure and platform reference
```

## 9. Architecture decision records *(generated)*

- [`0001-record-architecture-decisions.md`](docs/adr/0001-record-architecture-decisions.md) — ADR-0001: Record architecture decisions
- [`0002-staged-development-process.md`](docs/adr/0002-staged-development-process.md) — ADR-0002: Staged development process with hardening gates
- [`0003-target-architecture-x86_64.md`](docs/adr/0003-target-architecture-x86_64.md) — ADR-0003: Target architecture is x86_64
- [`0004-rust-implementation-language.md`](docs/adr/0004-rust-implementation-language.md) — ADR-0004: Rust (nightly-pinned) with minimal assembly
- [`0005-capability-based-microkernel.md`](docs/adr/0005-capability-based-microkernel.md) — ADR-0005: Capability-based microkernel baseline
- [`0006-boot-and-test-tooling.md`](docs/adr/0006-boot-and-test-tooling.md) — ADR-0006: Limine boot, QEMU testing, GitHub Actions CI
- [`0007-stable-rust-toolchain.md`](docs/adr/0007-stable-rust-toolchain.md) — ADR-0007: Pinned stable Rust toolchain
- [`0008-stage-1-scope.md`](docs/adr/0008-stage-1-scope.md) — ADR-0008: Stage 1 scope — boot, announce, power off / boot to UEFI setup
- [`0009-uefi-runtime-services.md`](docs/adr/0009-uefi-runtime-services.md) — ADR-0009: UEFI Runtime Services via a virtual window and Memory Attributes Table
- [`0010-power-off-path.md`](docs/adr/0010-power-off-path.md) — ADR-0010: Power-off strategy
- [`0011-ps2-polling-input.md`](docs/adr/0011-ps2-polling-input.md) — ADR-0011: PS/2 polling input for Stage 1
- [`0012-limine-pin-and-base-revision.md`](docs/adr/0012-limine-pin-and-base-revision.md) — ADR-0012: Limine v11 pinned by commit and checksum; base revision 5
- [`0013-stage-2-scope.md`](docs/adr/0013-stage-2-scope.md) — ADR-0013: Stage 2 scope — Bluetooth input and wired LAN internet
- [`0014-exception-handling-and-stacks.md`](docs/adr/0014-exception-handling-and-stacks.md) — ADR-0014: CPU exception handling, IST stacks and guard-paged kernel stacks
- [`0015-placeholder-monochrome-ui.md`](docs/adr/0015-placeholder-monochrome-ui.md) — ADR-0015: Placeholder monochrome UI
- [`0016-target-hardware-inventory.md`](docs/adr/0016-target-hardware-inventory.md) — ADR-0016: Target hardware inventory and input/Bluetooth priorities
- [`0017-bluetooth-first-internal-controller.md`](docs/adr/0017-bluetooth-first-internal-controller.md) — ADR-0017: Bluetooth is the primary input path; internal MediaTek controller first
- [`0018-mediatek-mt7961-bluetooth-target.md`](docs/adr/0018-mediatek-mt7961-bluetooth-target.md) — ADR-0018: MediaTek MT7961 (MT7921K) Bluetooth — concrete target and firmware handling
- [`0019-ble-hid-first-and-validation-targets.md`](docs/adr/0019-ble-hid-first-and-validation-targets.md) — ADR-0019: BLE HID first; validation targets from the owner's Linux logs
- [`0020-memory-management.md`](docs/adr/0020-memory-management.md) — ADR-0020: Physical memory manager, kernel-owned address space and heap
- [`0021-interrupts-and-timekeeping.md`](docs/adr/0021-interrupts-and-timekeeping.md) — ADR-0021: Interrupt architecture and timekeeping
- [`0022-project-vision-and-end-goal.md`](docs/adr/0022-project-vision-and-end-goal.md) — ADR-0022: Project vision and end goal
- [`0023-acpi-pci-and-untrusted-parsers.md`](docs/adr/0023-acpi-pci-and-untrusted-parsers.md) — ADR-0023: ACPI/PCI discovery, untrusted-data parsers and the AML decision
- [`0024-usb-xhci-and-hid.md`](docs/adr/0024-usb-xhci-and-hid.md) — ADR-0024: USB stack — xHCI driver, enumeration, hubs, HID, and the on-screen kernel log
- [`0025-bluetooth-host-stack.md`](docs/adr/0025-bluetooth-host-stack.md) — ADR-0025: Bluetooth host stack — design, security decisions and verification boundary
- [`0026-release-process-and-versioning.md`](docs/adr/0026-release-process-and-versioning.md) — ADR-0026: Release process and versioning (X.Y.Z)
- [`0027-licensing.md`](docs/adr/0027-licensing.md) — ADR-0027: Licensing — Apache-2.0
- [`0028-networking-stack.md`](docs/adr/0028-networking-stack.md) — ADR-0028: Networking: smoltcp stack, igc/igb and virtio-net drivers
- [`0029-structure-document-for-agents.md`](docs/adr/0029-structure-document-for-agents.md) — ADR-0029: STRUCTURE.md — a generated platform reference for coding agents
- [`0030-security-review-1.md`](docs/adr/0030-security-review-1.md) — ADR-0030: Security review 1 — scan results and dispositions
