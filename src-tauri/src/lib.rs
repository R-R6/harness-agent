// 会话数据代理：复用 mcp-lab 的 agent-sessions-mcp (server.js)（独立 crate：crates/session_proxy）
use session_proxy::{SessionInfo, TranscriptEntry};

/// 列出会话（agent 可选：claude / codex，不传则两者）
#[tauri::command]
fn list_sessions(agent: Option<String>) -> Result<Vec<SessionInfo>, String> {
    session_proxy::list_sessions(agent)
}

/// 读取某会话正文（tail：只取末尾 N 条，默认 200）
#[tauri::command]
fn get_transcript(file: String, tail: Option<i64>) -> Result<Vec<TranscriptEntry>, String> {
    session_proxy::get_transcript(&file, tail)
}

/// 按关键词全文搜索会话
#[tauri::command]
fn search_sessions(keyword: String) -> Result<Vec<SessionInfo>, String> {
    session_proxy::search_sessions(&keyword)
}

// 脚手架自带示例 command（阶段 1 完成后移除）
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            list_sessions,
            get_transcript,
            search_sessions
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
