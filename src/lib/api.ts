// 调用 Rust 侧 tauri commands（对应 lib.rs 的 list_sessions / get_transcript / search_sessions）
import { invoke } from "@tauri-apps/api/core";
import type { SessionInfo, TranscriptEntry } from "./types";

export async function fetchSessions(agent?: string): Promise<SessionInfo[]> {
  return invoke<SessionInfo[]>("list_sessions", agent ? { agent } : {});
}

export async function fetchTranscript(
  file: string,
  tail?: number,
): Promise<TranscriptEntry[]> {
  return invoke<TranscriptEntry[]>("get_transcript", { file, tail });
}

export async function searchSessions(keyword: string): Promise<SessionInfo[]> {
  return invoke<SessionInfo[]>("search_sessions", { keyword });
}
