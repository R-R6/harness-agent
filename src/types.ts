// 与 Rust 侧 session_proxy crate 的数据结构对齐（契约）
export interface SessionInfo {
  agent: string;
  agentLabel: string;
  file: string;
  title?: string; // 会话标题，空则回退文件名
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
}
