# Architecture Decision Records

Process: [ADR-0001](0001-record-architecture-decisions.md). Template: [template.md](template.md).

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-staged-development-process.md) | Staged development process with hardening gates | Accepted |
| [0003](0003-target-architecture-x86_64.md) | Target architecture is x86_64 | Accepted |
| [0004](0004-rust-implementation-language.md) | Rust with minimal assembly | Superseded by 0007 (toolchain only) |
| [0005](0005-capability-based-microkernel.md) | Capability-based microkernel baseline | Accepted |
| [0006](0006-boot-and-test-tooling.md) | Limine boot, QEMU testing, GitHub Actions CI | Accepted |
| [0007](0007-stable-rust-toolchain.md) | Pinned stable Rust toolchain | Accepted |
| [0008](0008-stage-1-scope.md) | Stage 1 scope: boot, announce, power off / boot to UEFI | Accepted |
| [0009](0009-uefi-runtime-services.md) | UEFI Runtime Services via virtual window and Memory Attributes Table | Accepted |
| [0010](0010-power-off-path.md) | Power-off strategy | Accepted |
| [0011](0011-ps2-polling-input.md) | PS/2 polling input for Stage 1 | Accepted |
| [0012](0012-limine-pin-and-base-revision.md) | Limine v11 pinned by commit and checksum; base revision 5 | Accepted |
| [0013](0013-stage-2-scope.md) | Stage 2 scope: Bluetooth input and wired LAN internet | Accepted |
| [0014](0014-exception-handling-and-stacks.md) | CPU exception handling, IST stacks and guard-paged kernel stacks | Accepted |
| [0015](0015-placeholder-monochrome-ui.md) | Placeholder monochrome UI | Accepted (provisional) |
| [0016](0016-target-hardware-inventory.md) | Target hardware inventory and input/Bluetooth priorities | Accepted |
| [0017](0017-bluetooth-first-internal-controller.md) | Bluetooth is the primary input path; internal MediaTek controller first | Accepted |
| [0018](0018-mediatek-mt7961-bluetooth-target.md) | MediaTek MT7961 (MT7921K) Bluetooth: concrete target and firmware handling | Accepted |
| [0019](0019-ble-hid-first-and-validation-targets.md) | BLE HID first; validation targets from the owner's Linux logs | Accepted |
| [0020](0020-memory-management.md) | Physical memory manager, kernel-owned address space and heap | Accepted |
