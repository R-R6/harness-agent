# Responsive Session List Layout Design

Date: 2026-08-15
Status: Approved

## Context

The session sidebar defaults to 320px wide. Its Claude column uses a percentage
width while the Codex column is sized from its content. Long Codex session rows
therefore consume more horizontal space and compress Claude into an unreadable
strip.

## Goals

- Keep Claude and Codex visible at the same time.
- Keep the two lists independently scrollable.
- Preserve search, counts, selection, keyboard navigation, favorites, context
  menus, export, and the desktop column-resize interaction.
- Prevent either group header or list from overlapping, regardless of sidebar
  width.

## Layout Rules

- The session-list container becomes an inline-size query container.
- At 420px and wider, both agent columns use controlled flex bases. The left
  column follows the existing drag position and the right column receives the
  remaining width. The drag range is additionally bounded by a 160px minimum
  readable width for each column. Long list content may truncate only inside
  its own row; it cannot resize either column.
- Below 420px, the container switches to a vertical split. Claude occupies the
  upper half and Codex the lower half, with both panels retaining a title and
  independent scroll region.
- The between-column drag handle is hidden in the vertical layout because
  horizontal resizing no longer has meaning there.
- Group labels do not wrap within the wide layout. The narrow layout provides
  enough horizontal space for each complete label instead of squeezing it.

## Scope

This is a presentation-only correction. No session data, filter behavior,
terminal behavior, or user action is added, removed, or redefined.

## Verification

- A regression test verifies the markup exposes stable first-column and
  resizer hooks used by the responsive rules.
- A CSS-focused test verifies the equal-width wide rule and vertical narrow
  rule, including hidden resize handle.
- Manual visual checks use a 320px sidebar for vertical grouping and a 480px
  sidebar for adjustable side-by-side columns.
