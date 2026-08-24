# Harness Vite Reuse Startup Design

Date: 2026-08-15
Status: Approved design, pending written-spec review

## Problem

`tauri dev` runs `npm run dev` through `beforeDevCommand`. When an existing
Harness Vite server already owns `127.0.0.1:14200`, the second Vite process
fails with `EADDRINUSE` and Tauri never starts the desktop window.

The launcher must not terminate a process merely because it owns the port.

## Goals

- Keep `npm run tauri dev` as the normal development command.
- Reuse an already-running Harness Vite server at the configured development
  URL.
- Start Vite normally when the development server is absent.
- Refuse to reuse an unrelated HTTP service on the configured port.
- Leave production builds, application behavior, and the fixed port `14200`
  unchanged.

## Design

Add a small Node launcher at `scripts/ensure-vite-dev.mjs` and set Tauri's
`beforeDevCommand` to run it.

1. The launcher probes `http://127.0.0.1:14200/` with a short timeout.
2. A response is reusable only when it identifies the Harness development
   page, using the document title and Vite entrypoint marker.
3. For a verified Harness response, the launcher exits successfully. Tauri
   then starts Cargo and opens the desktop window against the existing server.
4. For a refused connection or timeout, the launcher starts `npm run dev` and
   forwards its standard input, output, errors, and termination signals.
5. For a reachable but non-Harness response, the launcher exits with a clear
   conflict error instead of binding over or terminating another service.

## Error Handling

- A checked Harness page is reused without changing its process.
- An unavailable endpoint causes a normal Vite start.
- An unrelated listener produces an actionable error that names the URL and
  leaves that listener untouched.
- Child-process start failures retain the existing Vite error output.

## Verification

- With no service on port `14200`, `npm run tauri dev` starts Vite and the
  desktop window.
- With an existing Harness Vite page on port `14200`, the same command opens
  the desktop window without an `EADDRINUSE` error.
- With a non-Harness HTTP page on port `14200`, the command exits without
  stopping or replacing that service.

## Scope

This change adds no user-facing feature and removes none. It only makes the
development launcher safe around an existing Harness Vite process.
