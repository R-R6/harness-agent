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
