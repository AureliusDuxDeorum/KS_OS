# KS_OS

A custom capability-based microkernel operating system for x86_64, written in Rust
(plus minimal assembly). This repository is the construction site: source, build
tooling, and the decision record.

## Where things live

| Path | Purpose |
| --- | --- |
| `docs/adr/` | Architecture Decision Records — the source of truth for *why* |
| `docs/ROADMAP.md` | Stages → phases → steps, with status |
| `CONTRIBUTING.md` | Commit rules and how outside changes are integrated |

## Development model

Work is organised as **Stages → Phases → Steps**. A **security hardening gate**
sits between every pair of stages. See
[ADR-0002](docs/adr/0002-staged-development-process.md).
