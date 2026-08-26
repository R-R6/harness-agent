// 与 Rust 侧 session_proxy crate 的数据结构对齐（契约）
export interface SessionInfo {
  agent: string;
  agentLabel: string;
  file: string;
  title?: string; // 会话标题，空则回退文件名
  /** 会话的原始工作目录（Claude JSONL 记录；Codex 无则缺省） */
  cwd?: string;
  updated: string;
}

export interface TranscriptEntry {
  type: string;
  text: string;
  at?: string;
}

// ---- 监督闭环（阶段 2） ----

export interface SuperviseRequest {
  task: string;
  work_dir: string;
  level?: string;
  max_rounds?: number;
  model?: string;
  mock?: boolean;
}

// ---- 任务注册表（阶段 B）----

export type TaskStatus = "running" | "accepted" | "rejected" | "cancelled" | "aborted";
export type TaskKind = "ps1" | "engine";

export interface TaskInfo {
  id: string;
  work_dir: string;
  kind: TaskKind;
  status: TaskStatus;
  rounds: number;
  last_reason: string;
  started_at_ms: number;
  /** 原始任务描述 */
  task?: string;
  /** 可重试审查时的会话文件路径 */
  session_file?: string;
  max_rounds?: number;
  model?: string;
  mock?: boolean;
}

// ---- MCP 健康检查（阶段 3） ----

export interface McpCheckItem {
  name: string;
  ok: boolean;
  detail: string;
}

export interface McpStatus {
  config_path: string;
  items: McpCheckItem[];
  server_js_exists: boolean;
  host_exe_exists: boolean;
  handshake_ok: boolean;
}

export interface McpFixResult {
  backup_path?: string;
  fixed_items: string[];
  ok: boolean;
  message: string;
}

export interface ReviewArtifact {
  round: number;
  verdict: string; // PASS / REVIEW
  reason: string;
  model: string;
  session_id: string;
  /** 会话 JSONL 完整路径（产物未携带时为空，跳转按钮隐藏） */
  file?: string;
}

// ---- 本机 CLI 终端工作台 ----

export type TerminalAgent = "claude" | "codex";
export type TerminalStatus = "idle" | "starting" | "running" | "stopping" | "exited" | "error";

export interface TerminalStartRequest {
  agent: TerminalAgent;
  work_dir: string;
  cols: number;
  rows: number;
  /** 额外 CLI 参数（续聊：claude --resume <id> / codex resume <id>） */
  args?: string[];
}

export interface TerminalSessionInfo {
  id: string;
  agent: TerminalAgent;
  work_dir: string;
  status: TerminalStatus;
  pid?: number;
}
