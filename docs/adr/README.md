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
