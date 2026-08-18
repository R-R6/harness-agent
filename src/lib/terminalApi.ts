import { invoke } from "@tauri-apps/api/core";
import type { TerminalSessionInfo, TerminalStartRequest } from "../types";

/** Start one local, user-authenticated CLI inside a native PTY. */
export function startTerminal(request: TerminalStartRequest): Promise<TerminalSessionInfo> {
  return invoke<TerminalSessionInfo>("start_terminal", { request });
}

export function writeTerminal(sessionId: string, data: string): Promise<void> {
  return invoke<void>("write_terminal", { sessionId, data });
}

export function resizeTerminal(sessionId: string, cols: number, rows: number): Promise<void> {
  return invoke<void>("resize_terminal", { sessionId, cols, rows });
}

export function stopTerminal(sessionId: string): Promise<void> {
  return invoke<void>("stop_terminal", { sessionId });
}
