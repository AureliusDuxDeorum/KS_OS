# Roadmap

Legend: ☐ not started · ◐ in progress · ☑ done. IDs: `S<stage>.P<phase>.<step>`.
Each stage ends with a **Hardening gate** ([ADR-0002](adr/0002-staged-development-process.md)).

## Stage 0 — Project foundation
- ☑ S0.P1: Governance — ADRs, contributing rules, roadmap

## Stage 1 — Boot, "OS operating", Power off / Boot into UEFI ([ADR-0008](adr/0008-stage-1-scope.md))
- ☑ S1.P1 Toolchain & build — pinned stable Rust, workspace, linker script, `cargo xtask` ([ADR-0007](adr/0007-stable-rust-toolchain.md))
- ☑ S1.P2 Image & boot — GPT + FAT ESP image, pinned/verified Limine ([ADR-0012](adr/0012-limine-pin-and-base-revision.md))
- ☑ S1.P3 Kernel entry — Limine bindings, serial console, panic handler, `OS operating` on serial and framebuffer
- ☑ S1.P4 Input & UI — PS/2 keyboard/mouse polling, framebuffer UI with two buttons ([ADR-0011](adr/0011-ps2-polling-input.md))
- ☑ S1.P5 Power off — ACPI S5 with UEFI and emulator fallbacks ([ADR-0010](adr/0010-power-off-path.md))
- ☑ S1.P6 Boot into UEFI — runtime services, `SetVirtualAddressMap`, `OsIndications` ([ADR-0009](adr/0009-uefi-runtime-services.md))
- ☑ S1.P7 Verification — QEMU/OVMF end-to-end tests, GitHub Actions CI
- ◐ **Gate G1:** hardening review — see [`hardening/stage-1.md`](hardening/stage-1.md) (open items listed there)

## Stage 2 — Bluetooth input and LAN internet ([ADR-0013](adr/0013-stage-2-scope.md))
Target hardware: Intel I225-V NIC, USB Bluetooth controller (Realtek/MediaTek/Broadcom/Qualcomm), Classic + BLE HID peripherals.

- ☑ S2.P1 Exceptions & CPU — GDT/TSS/IST, IDT, fault logging, guard-paged kernel stacks *(closes R2, R3; [ADR-0014](adr/0014-exception-handling-and-stacks.md))*
- ☑ S2.P0 Monochrome placeholder UI ([ADR-0015](adr/0015-placeholder-monochrome-ui.md))
- ☐ S2.P2 Memory — frame allocator, kernel-owned page tables (drop bootloader tables), heap, DMA allocator
- ☐ S2.P3 Interrupts & time — LAPIC/IOAPIC, timer, MSI/MSI-X, interrupt-driven PS/2
- ☐ S2.P4 ACPI & PCI — host-testable fuzzed table parsers *(closes R9)*, ECAM enumeration, AML decision *(R7)*
- ☐ S2.P5 USB xHCI — controller, enumeration, hubs, USB HID with full report-descriptor parser *(prerequisite for Bluetooth, since controllers are USB devices; HID parser shared with Bluetooth HID; [ADR-0016](adr/0016-target-hardware-inventory.md), [ADR-0017](adr/0017-bluetooth-first-internal-controller.md))*
- ☐ S2.P6 Bluetooth — USB HCI transport, vendor firmware loaders (internal MediaTek first, ASUS BT500 optional second; [ADR-0017](adr/0017-bluetooth-first-internal-controller.md)), HCI LE, SMP (LE Secure Connections), ATT/GATT, HID-over-GATT first ([ADR-0019](adr/0019-ble-hid-first-and-validation-targets.md)), then L2CAP/SDP, SSP pairing and Classic HID, bond handling, unified input events
- ☐ S2.P7 Networking — `igc` (I225-V) driver, Ethernet, smoltcp, DHCP, DNS, ICMP, TCP; virtio-net/e1000e for CI
- ☐ S2.P8 Integration — Bluetooth/network status and pairing screen in the UI, automated + hardware-in-the-loop tests
- ☐ **Gate G2:** hardening review (cannot close while G1 items R3, R8, R9 are open)

## Later stages (outline only; to be re-planned after Gate G2)
- Stage 3: Threads, scheduler, user mode, syscalls
- Stage 4: Capabilities and IPC
- Stage 5: Userspace services and driver isolation, storage, TLS
