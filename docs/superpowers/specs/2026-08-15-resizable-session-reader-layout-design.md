# Resizable Session Reader Layout Design

Date: 2026-08-15
Status: Approved through the existing developer-workbench UI baseline

## Problem

The session reader uses a five-pixel divider driven only by mouse events. It is
hard to target and has no keyboard alternative. Its sidebar can also grow until
the transcript panel is effectively unusable in a non-maximized window.

## Decision

- Use a 12px pointer-enabled splitter hit area with a persistent 1px divider
  and a visible hover, focus, and drag grip.
- Capture the pointer while dragging. The same splitter supports Left/Right,
  Home, and End keyboard adjustments and exposes its range with ARIA separator
  attributes.
- Bound the session sidebar to 260px through 440px and calculate its actual
  upper bound from the session layout width. The reader receives a target
  minimum of 420px whenever the window permits it.
- At constrained widths, prioritize the transcript: the sidebar contracts to
  its compact readable width, where the already-approved Claude/Codex vertical
  list layout takes over. The previous forced 45% sidebar rule is removed.

## Scope

No session data, selection, search, keyboard list navigation, context-menu
actions, transcript loading, or terminal functionality changes. The only new
interaction is an accessible version of the existing resize affordance.

## Verification

- An App regression test verifies pointer drag, keyboard resize, ARIA values,
  and the reader-reserving upper bound.
- A CSS regression test verifies the 12px splitter hit area, container-aware
  reader minimum, and removal of the competing 45% narrow-width rule.
- Run the full test suite and production build.
