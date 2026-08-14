// 会话数据代理：复用 mcp-lab 的 agent-sessions-mcp (server.js)（独立 crate：crates/session_proxy）
use session_proxy::{SessionInfo, TranscriptEntry};
use supervise_runner::{ReviewArtifact, SuperviseRequest};

use std::collections::HashMap;
use std::io::BufRead;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

// ---------------- 监督进程状态（并发锁 + 进程表） ----------------

struct SuperviseState {
    /// task_id → 运行中的子进程（用于取消）
    running: Mutex<HashMap<String, std::process::Child>>,
    /// 正在跑监督任务的工作目录（并发锁：同目录只能跑一个）
    busy_workdirs: Mutex<Vec<String>>,
}

fn normalize_path(p: &str) -> String {
    // 去尾部斜杠 + 小写（Windows 路径大小写不敏感）
    p.trim_end_matches(['\\', '/']).to_lowercase()
}

// ---------------- 会话 commands（阶段 1） ----------------

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

// ---------------- 监督闭环 commands（阶段 2） ----------------

/// 启动监督闭环：spawn pwsh supervise.ps1，stdout 实时推送到前端
/// 返回 task_id；日志事件 supervise-log，结束事件 supervise-done
#[tauri::command]
async fn run_supervise(
    app: AppHandle,
    state: State<'_, SuperviseState>,
    request: SuperviseRequest,
) -> Result<String, String> {
    let work_dir = normalize_path(&request.work_dir);
    {
        let busy = state.busy_workdirs.lock().unwrap();
        if busy.contains(&work_dir) {
            return Err(format!("工作目录 {work_dir} 已有监督任务在运行"));
        }
    }

    let mut child = supervise_runner::spawn_supervise(&request)?;
    let task_id = format!(
        "task-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
    {
        state.running.lock().unwrap().insert(task_id.clone(), child);
        state.busy_workdirs.lock().unwrap().push(work_dir.clone());
    }

    // 后台线程读 stdout → 逐行 emit 到前端；进程结束后清理 State 并 emit done
    let app2 = app.clone();
    let task_id2 = task_id.clone();
    let work_dir2 = work_dir.clone();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(l) = line {
                let _ = app2.emit(
                    "supervise-log",
                    serde_json::json!({ "taskId": task_id2, "line": l }),
                );
            }
        }
        // stdout EOF（进程退出）→ 收尾
        let running = app2.state::<SuperviseState>();
        {
            let mut running_map = running.running.lock().unwrap();
            if let Some(mut c) = running_map.remove(&task_id2) {
                let _ = c.wait();
            }
        }
        running
            .busy_workdirs
            .lock()
            .unwrap()
            .retain(|w| w != &work_dir2);
        let _ = app2.emit("supervise-done", serde_json::json!({ "taskId": task_id2 }));
    });

    Ok(task_id)
}

/// 取消运行中的监督任务（kill 子进程）
#[tauri::command]
async fn cancel_supervise(app: AppHandle, task_id: String) -> Result<(), String> {
    let state = app.state::<SuperviseState>();
    let mut running = state.running.lock().unwrap();
    if let Some(mut c) = running.remove(&task_id) {
        let _ = c.kill();
        let _ = c.wait();
        return Ok(());
    }
    Err("任务不存在或已结束".into())
}

/// 读取监督闭环产物（.supervise 目录）
#[tauri::command]
async fn read_review_artifacts(work_dir: String) -> Result<Vec<ReviewArtifact>, String> {
    supervise_runner::read_artifacts(&work_dir)
}

/// MCP 注册健康检查（toml 结构化解析 + 真实握手）
#[tauri::command]
async fn check_mcp() -> Result<mcp_checker::McpStatus, String> {
    Ok(mcp_checker::check_mcp(mcp_checker::DEFAULT_CONFIG))
}

/// MCP 一键修复（备份 + 最小侵入插入缺失项 + 原子写）
#[tauri::command]
async fn fix_mcp() -> Result<mcp_checker::FixResult, String> {
    Ok(mcp_checker::fix_mcp(mcp_checker::DEFAULT_CONFIG))
}

// ---------------- 入口 ----------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(SuperviseState {
            running: Mutex::new(HashMap::new()),
            busy_workdirs: Mutex::new(vec![]),
        })
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            get_transcript,
            search_sessions,
            run_supervise,
            cancel_supervise,
            read_review_artifacts,
            check_mcp,
            fix_mcp,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
