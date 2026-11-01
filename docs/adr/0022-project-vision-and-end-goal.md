# ADR-0022: Project vision and end goal

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner
- **Stage/Phase:** cross-cutting (governs all stages)

## Context
Earlier ADRs scoped individual stages. The owner has now defined the end goal of the project.

## Decision
The end goal of KS_OS is:

> A fully capable operating system **comparable to Ubuntu in polish**, with a **cybercore,
> monochrome design philosophy based on KS branding** (references, logos and exact guidelines
> to be supplied with S2.P8), and **access comparable to Arch**.
>
> **Until this point is reached, the project remains open.** No stage count, version number or
> milestone closes the project.

### How we interpret it (provisional — the owner may correct any line)
- **Polish "comparable to Ubuntu"**: installable from media, boots to a graphical login and
  desktop, works on common real hardware (display, input incl. Bluetooth, audio, networking
  incl. Wi-Fi, storage, power management, suspend), ships a coherent default application set,
  receives updates, and its failure modes are handled gracefully.
- **Access "comparable to Arch"**: the user owns the machine — a minimal base they assemble,
  complete system access, a documented and scriptable system, a package/build ecosystem
  they can extend and a rolling-release model; no vendor lock-down, no hidden defaults.
- **Design**: cybercore monochrome (black/white/gray as in ADR-0015), KS branding applied
  consistently across boot, login, shell and applications once the guidelines arrive.

### Consequences for process
- Stages continue to be defined one at a time with owner direction (ADR-0002); each ends in
  a hardening gate. The roadmap's later stages are an **outline only** and will be revised.
- Every ADR from now on is checked against this goal.

## Open architectural questions (each needs an owner-informed ADR before it is built)
1. **Application compatibility.** "Ubuntu-level" usefulness implies running substantial existing
   software. Options: native ports against a POSIX/libc layer; a Linux-syscall compatibility layer;
   a Wayland/Mesa-style graphics stack; or an own toolkit. This is the most consequential choice
   and interacts with ADR-0005 (capability microkernel).
2. **Security model vs Arch-like access.** How capabilities are exposed to a user who can do
   "anything" on their own machine without the system being unsafe by default.
3. **Storage and filesystems** (native vs ext4/btrfs/NTFS support), boot and update integrity.
4. **Graphics**: GPU drivers (vendor-specific, large), compositor, HiDPI, fonts and text shaping.
5. **Package management, build system and self-hosting** of the toolchain.
6. **Hardware coverage strategy**, including Wi-Fi, audio, GPUs, suspend/resume.
7. **Licensing** of the kernel and system components.
8. **Installer, update and recovery** story.

## Honest scope note
This is a multi-year, multi-person-scale goal; comparable projects take large teams a decade or
more. The staged, test-driven process with hardening gates is chosen so that every increment is
verifiable and nothing foundational is deferred. Shortcuts that would foreclose the end goal
(e.g. throw-away drivers) are avoided; shortcuts that merely defer scope are recorded as ADRs.
