// 会话数据代理：复用 mcp-lab 的 agent-sessions-mcp (server.js)（独立 crate：crates/session_proxy）
use session_proxy::{SessionInfo, TranscriptEntry};
use supervise_runner::{ReviewArtifact, SuperviseRequest};

use std::collections::HashMap;
use std::io::BufRead;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

/// 监督任务 id 自增计数器（避免毫秒时间戳碰撞）
static TASK_COUNTER: AtomicU64 = AtomicU64::new(1);

// ---------------- 监督进程状态（并发锁 + 进程表） ----------------

struct SuperviseState {
    /// task_id → 运行中的子进程（用于取消）
    running: Mutex<HashMap<String, std::process::Child>>,
    /// 正在跑监督任务的工作目录（并发锁：同目录只能跑一个）
    busy_workdirs: Mutex<Vec<String>>,
}

/// 规范化路径用于并发锁比较：解析 `.`/`..` + 去尾部斜杠 + 小写（Windows 大小写不敏感）
fn normalize_path(p: &str) -> String {
    let p = p.trim_end_matches(['\\', '/']);
    if p.is_empty() {
        return String::new();
    }
    let mut out = std::path::PathBuf::new();
    for comp in std::path::Path::new(p).components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().to_lowercase()
}

// ---------------- 会话 commands（阶段 1） ----------------

/// 列出会话（agent 可选：claude / codex；limit 可选：每 agent 条数，默认 20 上限 200）
#[tauri::command]
fn list_sessions(agent: Option<String>, limit: Option<usize>) -> Result<Vec<SessionInfo>, String> {
    session_proxy::list_sessions(agent, limit)
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
    if work_dir.is_empty() {
        return Err("工作目录不能为空".into());
    }
    if !std::path::Path::new(&request.work_dir).is_dir() {
        return Err(format!("工作目录不存在: {}", request.work_dir));
    }
    {
        let busy = state.busy_workdirs.lock().unwrap();
        if busy.contains(&work_dir) {
            return Err(format!("工作目录 {work_dir} 已有监督任务在运行"));
        }
    }

    let mut child = supervise_runner::spawn_supervise(&request)?;
    let task_id = format!("task-{}", TASK_COUNTER.fetch_add(1, Ordering::Relaxed));
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
        // stdout EOF（进程退出）→ 收尾：wait 拿退出码，清理 State
        let running = app2.state::<SuperviseState>();
        let exit_code: Option<i64> = {
            let mut running_map = running.running.lock().unwrap();
            if let Some(mut c) = running_map.remove(&task_id2) {
                c.wait().ok().and_then(|s| s.code()).map(|c| c as i64)
            } else {
                None // 已被 cancel 提前移除（进程是 kill 掉的）
            }
        };
        running
            .busy_workdirs
            .lock()
            .unwrap()
            .retain(|w| w != &work_dir2);
        let _ = app2.emit(
            "supervise-done",
            serde_json::json!({ "taskId": task_id2, "exitCode": exit_code }),
        );
    });

    Ok(task_id)
}

/// 取消运行中的监督任务（kill 整个进程树：pwsh + claude/codex/node 子进程）
#[tauri::command]
async fn cancel_supervise(app: AppHandle, task_id: String) -> Result<(), String> {
    let state = app.state::<SuperviseState>();
    let mut running = state.running.lock().unwrap();
    if let Some(mut c) = running.remove(&task_id) {
        let pid = c.id();
        let _ = c.kill();
        let _ = c.wait();
        // 级联清理子进程：taskkill /T（失败无妨——主进程已杀，且某些环境无 taskkill）
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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

/// 导出会话正文为 Markdown 文件（右键菜单功能）
#[tauri::command]
async fn export_transcript_md(file: String, dest: String) -> Result<String, String> {
    // 边界校验：dest 必须是绝对路径 + .md/.markdown 后缀（防任意路径写入）
    let dest_path = std::path::Path::new(&dest);
    if !dest_path.is_absolute() {
        return Err("导出路径必须是绝对路径".into());
    }
    let ext = dest_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !matches!(ext.as_str(), "md" | "markdown") {
        return Err("导出文件必须是 .md 或 .markdown 后缀".into());
    }
    let entries = session_proxy::get_transcript(&file, None).map_err(|e| e.to_string())?;
    let mut md = String::from("# 会话导出\n\n");
    for e in &entries {
        let t = match e.msg_type.as_str() {
            "user" => "🧑 用户",
            "assistant" => "🤖 助手",
            "title" => "📌 标题",
            other => other,
        };
        md.push_str(&format!("## {t}\n\n{}\n\n", e.text));
    }
    std::fs::write(&dest, md).map_err(|e| format!("写入失败: {e}"))?;
    Ok(dest)
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
            export_transcript_md,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
