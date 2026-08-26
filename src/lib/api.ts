// 调用 Rust 侧 tauri commands（对应 lib.rs 的 list_sessions / get_transcript / search_sessions）
import { invoke } from "@tauri-apps/api/core";
import type {
  McpFixResult,
  McpStatus,
  ReviewArtifact,
  SessionInfo,
  SuperviseRequest,
  TaskInfo,
  TranscriptEntry,
} from "../types";

export async function fetchSessions(agent?: string, limit?: number): Promise<SessionInfo[]> {
  const args: Record<string, string | number> = {};
  if (agent) args.agent = agent;
  if (limit) args.limit = limit;
  return invoke<SessionInfo[]>("list_sessions", args);
}

export async function fetchTranscript(
  file: string,
  tail?: number,
  offset?: number,
): Promise<TranscriptEntry[]> {
  const args: Record<string, string | number> = { file };
  if (tail !== undefined) args.tail = tail;
  if (offset !== undefined) args.offset = offset;
  return invoke<TranscriptEntry[]>("get_transcript", args);
}

export async function searchSessions(keyword: string): Promise<SessionInfo[]> {
  return invoke<SessionInfo[]>("search_sessions", { keyword });
}

// ---- 监督闭环（阶段 2） ----

/** 启动监督闭环（无头 ps1 模式），返回 task_id */
export async function runSupervise(req: SuperviseRequest): Promise<string> {
  return invoke<string>("run_supervise", { request: req });
}

/** 启动终端驱动监督（阶段 2：任务注入运行中的 Claude 终端 pane），返回 task_id */
export async function runSuperviseTerminal(req: SuperviseRequest): Promise<string> {
  return invoke<string>("run_supervise_terminal", { request: req });
}

/** 取消运行中的监督任务 */
export async function cancelSupervise(taskId: string): Promise<void> {
  return invoke<void>("cancel_supervise", { taskId });
}

/** 重试审查：复用已中止任务的会话，跳过工人只再跑 Codex */
export async function retrySuperviseReview(taskId: string): Promise<string> {
  return invoke<string>("retry_supervise_review", { taskId });
}

/** 读 .supervise 产物（审查看板数据）。无头任务传 taskId 读 tasks/<id>/，省略则读根目录。 */
export async function fetchReviewArtifacts(workDir: string, taskId?: string | null): Promise<ReviewArtifact[]> {
  const args: Record<string, string> = { workDir };
  if (taskId) args.taskId = taskId;
  return invoke("read_review_artifacts", args);
}

/** 列出全部监督任务（含历史终态；重启后清空） */
export async function fetchTasks(): Promise<TaskInfo[]> {
  return invoke<TaskInfo[]>("list_supervise_tasks");
}

// ---- MCP 健康检查 ----

export async function checkMcp(): Promise<McpStatus> {
  return invoke("check_mcp");
}

export async function fixMcp(): Promise<McpFixResult> {
  return invoke("fix_mcp");
}

/** 导出会话正文为 Markdown（右键菜单） */
export async function exportTranscriptMd(file: string, dest: string): Promise<string> {
  return invoke("export_transcript_md", { file, dest });
}
