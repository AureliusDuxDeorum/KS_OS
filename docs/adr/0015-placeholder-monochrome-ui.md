# ADR-0015: Placeholder monochrome UI

- **Status:** Accepted (provisional — to be superseded by a real design language)
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S2 (UI)

## Context
The Stage 1 UI used red/blue/green accents chosen ad hoc. The owner wants a neutral
placeholder until a real design philosophy is defined.

## Decision
Use **black, white and gray only**. Severity is conveyed by form rather than colour: plain
text (info), `!`-prefixed light gray (warning), inverted white bar with `ERROR:` (error).
Buttons: dark fill with gray border at rest; lighter fill on hover; white border (thicker)
for keyboard focus; fully inverted while pressed; dark-gray text and border when disabled.
The fatal-fault screen uses the same palette.

## Consequences
Accessible by construction (contrast and shape, not hue). All colours live in constants at the
top of `ui.rs` so the eventual design language replaces them in one place.
