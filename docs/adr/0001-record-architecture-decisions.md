# ADR-0001: Record architecture decisions

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude (primary maintainer)
- **Stage/Phase:** cross-cutting

## Context
Several parties may change this repository. Without a shared, durable record of
decisions, intent is lost and work drifts.

## Decision
We keep Architecture Decision Records in `docs/adr/`, numbered sequentially, using
`template.md`. The index in `docs/adr/README.md` and `docs/ROADMAP.md` are kept
current in the same change that alters a decision. Accepted ADRs are immutable
apart from their status line; reversals use a superseding ADR.

## Alternatives considered
- Wiki/issues only: not versioned with the code.
- Unstructured design docs: no status lifecycle, hard to audit.

## Consequences
Small overhead per significant change; strong traceability for every party.
