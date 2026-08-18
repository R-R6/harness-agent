# Unified Resizable Workbench Layout Design

Date: 2026-08-15
Status: Approved through the developer-workbench UI baseline and direct implementation authorization

## Goal

Every boundary that separates two functional work regions is adjustable. Purely
visual borders in toolbars, status bars, cards, fields, and message blocks stay
non-interactive.

## Resizable Boundaries

- Workspace navigation and workspace content: vertical, 208px to 320px while
  expanded. Collapsed navigation remains the existing fixed 72px rail.
- Session browser and transcript reader: vertical, preserving the transcript's
  target minimum reading width.
- Claude and Codex session groups: vertical when side by side and horizontal
  when stacked in a narrow session browser.
- Supervise configuration and review output: vertical on wide layouts and
  horizontal on compact layouts.
- Claude and Codex terminal panes: vertical on wide layouts and horizontal when
  the terminal workspace stacks.

## Interaction Contract

All boundaries use one shared `SplitHandle` component. It provides a 12px hit
area around a one-pixel visual rule, pointer capture, drag feedback, correct
row/column resize cursors, and keyboard alternatives. Vertical separators use
Left/Right; horizontal separators use Up/Down; Home and End select the allowed
minimum and maximum. Each exposes orientation and numeric values through ARIA.

The leading pane is clamped so both regions retain their minimum usable size.
Resize values are saved locally and restored on the next application launch.
Responsive orientation changes keep separate horizontal and vertical values so
one layout does not corrupt the other.

## Scope

No session, transcript, terminal, supervision, MCP, navigation, or context-menu
behavior is removed or redefined. Decorative borders do not become draggable.

## Verification

- Shared component tests cover pointer and keyboard resizing in both axes.
- Integration tests verify each functional boundary is present and sized.
- Responsive tests verify session and terminal splitters change orientation.
- Full frontend tests and production build pass.
- `npm run tauri dev` is executed and the desktop layout is visually checked.
