# ADR-0005: Capability-based microkernel baseline

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** cross-cutting (design baseline)

## Context
Security is a first-class goal with a hardening gate between stages. A small
trusted computing base (TCB) makes audit and hardening tractable.

## Decision
The kernel provides only: address-space and memory management, thread
scheduling, IPC, interrupt delivery, and capability management. Drivers, file
systems, and network stacks run as unprivileged userspace services. All access to
kernel objects is mediated by unforgeable, revocable capabilities (no ambient
authority).

## Alternatives considered
- Monolithic: faster early progress, large attack surface.
- Hybrid: blurs the TCB boundary.

## Consequences
IPC performance becomes critical; more up-front design; userspace driver
framework needed before useful hardware support.
