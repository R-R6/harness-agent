// 调用 Rust 侧 tauri commands（对应 lib.rs 的 list_sessions / get_transcript / search_sessions）
import { invoke } from "@tauri-apps/api/core";
import type {
  McpFixResult,
  McpStatus,
  ReviewArtifact,
  SessionInfo,
  SuperviseRequest,
  TranscriptEntry,
} from "./types";

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

// ---- 监督闭环（阶段 2） ----

/** 启动监督闭环，返回 task_id */
export async function runSupervise(req: SuperviseRequest): Promise<string> {
  return invoke<string>("run_supervise", { request: req });
}

/** 取消运行中的监督任务 */
export async function cancelSupervise(taskId: string): Promise<void> {
  return invoke<void>("cancel_supervise", { taskId });
}

/** 读 .supervise 产物（审查看板数据） */
export async function fetchReviewArtifacts(workDir: string): Promise<ReviewArtifact[]> {
  return invoke("read_review_artifacts", { workDir });
}

// ---- MCP 健康检查 ----

export async function checkMcp(): Promise<McpStatus> {
  return invoke("check_mcp");
}

export async function fixMcp(): Promise<McpFixResult> {
  return invoke("fix_mcp");
}
