# ADR-0029: STRUCTURE.md — a generated platform reference for coding agents

- **Status:** Accepted
- **Date:** 2026-11-02
- **Deciders:** owner (request), Claude (maintainer)
- **Stage/Phase:** cross-cutting

## Context
The long-term goal includes running demanding developer tools such as coding agents on KS_OS, and in the
meantime other agents and people work on this repository. An assistant that does not know what the OS
provides (and what it does not) invents APIs. The owner wants one simple file that can be handed to any system.

## Decision
`STRUCTURE.md` at the repository root is the single hand-over document: platform facts, what exists, what is
missing, enforced conventions, commands, a driver recipe, plus generated sections (crate table, annotated
file tree with each file's first doc line, ADR index). The hand-written text lives in
`xtask/structure/STRUCTURE.md.in`; `cargo xtask structure` regenerates and `cargo xtask structure --check`
fails CI when it is stale, so it cannot silently drift. Updating it is part of every phase's definition of done.

## Alternatives considered
- *Hand-maintained file only*: drifts within a phase. *Generated file only*: cannot carry platform facts.
- *`CLAUDE.md`/`AGENTS.md`*: tool-specific names; this file is deliberately vendor-neutral (a short
  `AGENTS.md` pointer can be added if a tool requires one).

## Consequences
Every structural change (new file, crate, ADR) requires re-running the generator; CI enforces it.
