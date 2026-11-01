# ADR-0011: PS/2 polling input for Stage 1

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S1

## Context
Stage 1 needs buttons that a user can actually press, before the kernel has an
IDT, interrupt controller, PCI enumeration or USB stack.

## Decision
Drive the i8042 controller by polling: keyboard (scancode set 1 via controller
translation) and 3-byte relative mouse packets. IRQs stay disabled in the
controller. The UI is keyboard-complete; the mouse is an addition.

## Consequences
- Works in QEMU/VMs and on hardware that exposes PS/2 or USB legacy emulation.
- **Limitation:** machines with USB-only input and no legacy emulation cannot
  press the buttons. USB HID (xHCI) is planned for a later stage.
- Replaced by interrupt-driven input once the IDT/APIC work lands (Stage 2).
