// 与 Rust 侧 session_proxy crate 的数据结构对齐（契约）
export interface SessionInfo {
  agent: string;
  agentLabel: string;
  file: string;
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
  level?: string; // L0/L1/L2
  max_rounds?: number; // >0 覆盖 Level
  model?: string; // 非空覆盖 Level
  mock?: boolean;
}

export interface ReviewArtifact {
  round: number;
  verdict: string; // PASS / REVIEW
  reason: string;
  model: string;
  session_id: string;
}
