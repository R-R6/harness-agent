// 会话数据代理：内置 agent-sessions-mcp (server.js)，资源路径由 setup 注入（独立 crate：crates/session_proxy）
use session_proxy::{SessionInfo, TranscriptEntry};
use supervise_runner::{ReviewArtifact, SuperviseRequest};
use terminal_host::{kill as kill_terminal_process, resize as resize_terminal_pty, spawn as spawn_terminal_pty, terminal_command, wait as wait_terminal_process, write_input as write_terminal_input};

use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

/// 监督任务 id 自增计数器（避免毫秒时间戳碰撞）
static TASK_COUNTER: AtomicU64 = AtomicU64::new(1);
static TERMINAL_COUNTER: AtomicU64 = AtomicU64::new(1);

// ---------------- 监督进程状态（并发锁 + 进程表） ----------------

struct SuperviseState {
    /// task_id → 运行中的子进程（用于取消）
    running: Mutex<HashMap<String, std::process::Child>>,
    /// 正在跑监督任务的工作目录（并发锁：同目录只能跑一个）
    busy_workdirs: Mutex<Vec<String>>,
}

impl SuperviseState {
    /// 窗口关闭时终止全部监督任务——否则 pwsh + claude/codex 孤儿进程
    /// 会在应用退出后继续无人监督地跑完整轮任务（烧 token）
    fn stop_all(&self) {
        let mut running = match self.running.lock() {
            Ok(r) => r,
            Err(_) => return,
        };
        for (_id, mut child) in running.drain() {
            let pid = child.id();
            let _ = kill_process_tree(Some(pid));
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ---------------- 本机 CLI 终端状态（ConPTY） ----------------

#[derive(Debug, Clone, serde::Deserialize)]
struct TerminalStartRequest {
    agent: String,
    work_dir: String,
    cols: u16,
    rows: u16,
    /// 额外 CLI 参数（续聊：claude --resume <id> / codex resume <id>）
    #[serde(default)]
    args: Option<Vec<String>>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct TerminalSessionInfo {
    id: String,
    agent: String,
    work_dir: String,
    status: String,
    pid: Option<u32>,
}

struct TerminalProcess {
    writer: std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
    master: std::sync::Arc<std::sync::Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    child: std::sync::Arc<std::sync::Mutex<Box<dyn portable_pty::Child + Send>>>,
    pid: Option<u32>,
}

struct TerminalState {
    sessions: Mutex<HashMap<String, TerminalProcess>>,
}

impl TerminalState {
    fn stop_all(&self) {
        if let Ok(sessions) = self.sessions.lock() {
            for process in sessions.values() {
                // 先杀进程树再兜底杀 PTY 主进程（顺序反了 taskkill 枚举不到子树）
                let _ = kill_process_tree(process.pid);
                let _ = kill_terminal_process(&process.child);
            }
        }
    }
}

/// Emit only complete UTF-8 code points. PTY reads can split Chinese characters
/// across chunks, while ANSI escape sequences remain safe to stream as-is.
fn emit_terminal_output(
    app: &AppHandle,
    session_id: &str,
    pending: &mut Vec<u8>,
) {
    loop {
        match std::str::from_utf8(pending) {
            Ok(data) => {
                if !data.is_empty() {
                    let _ = app.emit(
                        "terminal-output",
                        serde_json::json!({ "sessionId": session_id, "data": data }),
                    );
                }
                pending.clear();
                return;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    let data = std::str::from_utf8(&pending[..valid])
                        .expect("UTF-8 valid prefix reported by std::str::from_utf8");
                    let _ = app.emit(
                        "terminal-output",
                        serde_json::json!({ "sessionId": session_id, "data": data }),
                    );
                    pending.drain(..valid);
                    continue;
                }
                if let Some(invalid_len) = error.error_len() {
                    let replacement = String::from_utf8_lossy(&pending[..invalid_len]).to_string();
                    let _ = app.emit(
                        "terminal-output",
                        serde_json::json!({ "sessionId": session_id, "data": replacement }),
                    );
                    pending.drain(..invalid_len);
                    continue;
                }
                // The buffer ends in an incomplete multi-byte character. Keep it
                // until the next read rather than replacing it prematurely.
                return;
            }
        }
    }
}

/// taskkill /T /F 杀掉整棵进程树（含 PTY 主进程）。
/// 必须在父进程还活着时调用——父进程先死后 taskkill 枚举不到子树，
/// 子孙进程会变成孤儿继续存活。目标进程已退出（taskkill 报"没有找到进程"）视为成功。
fn kill_process_tree(pid: Option<u32>) -> Result<(), String> {
    let Some(pid) = pid else { return Ok(()) };
    #[cfg(windows)]
    {
        let output = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
            .map_err(|error| format!("taskkill 启动失败: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("没有找到进程") && !stderr.contains("not found") {
                return Err(format!("终止进程树失败（PID {pid}）: {}", stderr.trim()));
            }
        }
    }
    Ok(())
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

/// 读取某会话正文（tail：只取末尾 N 条，默认 200；offset：从末尾跳过 N 条，往前翻页）
#[tauri::command]
fn get_transcript(file: String, tail: Option<i64>, offset: Option<i64>) -> Result<Vec<TranscriptEntry>, String> {
    session_proxy::get_transcript(&file, tail, offset)
}

/// 按关键词全文搜索会话
#[tauri::command]
fn search_sessions(keyword: String) -> Result<Vec<SessionInfo>, String> {
    session_proxy::search_sessions(&keyword)
}

// ---------------- 本机 CLI 终端 commands（阶段 4） ----------------

/// 在 Windows ConPTY（或目标平台对应的 PTY）中启动用户本机 CLI。
#[tauri::command]
fn start_terminal(
    app: AppHandle,
    state: State<'_, TerminalState>,
    request: TerminalStartRequest,
) -> Result<TerminalSessionInfo, String> {
    if request.work_dir.trim().is_empty() {
        return Err("工作目录不能为空".into());
    }
    if !std::path::Path::new(&request.work_dir).is_dir() {
        return Err(format!("工作目录不存在: {}", request.work_dir));
    }
    let extra_args: Vec<String> = request
        .args
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !a.trim().is_empty())
        .collect();
    let (command, args) = terminal_command(&request.agent, &extra_args)?;
    let spawned = spawn_terminal_pty(
        &command,
        &args,
        &request.work_dir,
        request.cols,
        request.rows,
    )?;
    let id = format!("terminal-{}", TERMINAL_COUNTER.fetch_add(1, Ordering::Relaxed));
    let info = TerminalSessionInfo {
        id: id.clone(),
        agent: request.agent.clone(),
        work_dir: request.work_dir.clone(),
        status: "running".into(),
        pid: spawned.pid,
    };
    let process = TerminalProcess {
        writer: spawned.writer,
        master: spawned.master,
        child: spawned.child,
        pid: spawned.pid,
    };
    state.sessions.lock().map_err(|_| "终端状态锁已损坏")?.insert(id.clone(), process);

    let app2 = app.clone();
    let id2 = id.clone();
    std::thread::spawn(move || {
        let mut reader = spawned.reader;
        let mut buffer = [0u8; 8192];
        let mut pending_utf8 = Vec::new();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    pending_utf8.extend_from_slice(&buffer[..size]);
                    emit_terminal_output(&app2, &id2, &mut pending_utf8);
                }
                Err(error) => {
                    let _ = app2.emit(
                        "terminal-error",
                        serde_json::json!({ "sessionId": id2, "message": format!("读取终端输出失败: {error}") }),
                    );
                    break;
                }
            }
        }
        if !pending_utf8.is_empty() {
            let data = String::from_utf8_lossy(&pending_utf8).to_string();
            let _ = app2.emit(
                "terminal-output",
                serde_json::json!({ "sessionId": id2, "data": data }),
            );
        }
        let state = app2.state::<TerminalState>();
        // 克隆 child 句柄后立即释放全局锁：wait() 可能无限期阻塞，
        // 持锁等待会把其他终端的所有操作一起卡死
        let child = state
            .sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&id2).map(|process| process.child.clone()));
        let code = child.as_ref().and_then(|child| wait_terminal_process(child));
        let leftover_pid = state
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&id2))
            .map(|process| process.pid);
        if let Some(pid) = leftover_pid {
            let _ = kill_process_tree(pid);
        }
        let _ = app2.emit(
            "terminal-exit",
            serde_json::json!({ "sessionId": id2, "code": code }),
        );
    });

    Ok(info)
}

#[tauri::command]
fn write_terminal(state: State<'_, TerminalState>, session_id: String, data: String) -> Result<(), String> {
    let sessions = state.sessions.lock().map_err(|_| "终端状态锁已损坏")?;
    let process = sessions.get(&session_id).ok_or("终端会话不存在或已退出")?;
    write_terminal_input(&process.writer, data.as_bytes())
}

#[tauri::command]
fn resize_terminal(state: State<'_, TerminalState>, session_id: String, cols: u16, rows: u16) -> Result<(), String> {
    let sessions = state.sessions.lock().map_err(|_| "终端状态锁已损坏")?;
    let process = sessions.get(&session_id).ok_or("终端会话不存在或已退出")?;
    resize_terminal_pty(&process.master, cols, rows)
}

#[tauri::command]
fn stop_terminal(state: State<'_, TerminalState>, session_id: String) -> Result<(), String> {
    // 把会话整体移出并持有：函数结束时析构 master（HPCON）→ ClosePseudoConsole，
    // conhost 立即退出 → PTY reader 读到 EOF → 前端收到 terminal-exit 恢复界面。
    // 不能依赖 Windows 在 client 全退后自动清理 conhost（实测会残留，UI 永远卡"停止中"）。
    // 移出后 reader 线程也拿不到退出码（code=null），主动停止不再误报"CLI 异常退出"。
    let process = {
        let mut sessions = state.sessions.lock().map_err(|_| "终端状态锁已损坏")?;
        sessions.remove(&session_id).ok_or("终端会话不存在或已退出")?
    };
    // 先杀整棵进程树（父进程还活着，taskkill /T 才能枚举到全部子孙），再兜底杀 PTY 主进程。
    // taskkill 在进程树动态变化时（CLI 刚启动/正在退出）会对个别子进程报
    // ERROR_NOT_SUPPORTED 等错误，但树根通常已被终止——这种部分失败不阻断：
    // 兜底 TerminateProcess 收尾，drop process 关闭 ConPTY（conhost 退出 → reader EOF
    // → 前端恢复），残片由 Windows 自动清理。
    if let Err(error) = kill_process_tree(process.pid) {
        eprintln!("[terminal] 终止进程树（PID {}）失败: {error}", process.pid.unwrap_or(0));
    }
    kill_terminal_process(&process.child)?;
    Ok(())
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
    // 检查与登记必须同一次持锁完成：分成两段锁的话，并发启动同目录的两个
    // 任务都能通过检查（TOCTOU）。spawn 失败时回滚登记。
    {
        let mut busy = state.busy_workdirs.lock().unwrap();
        if busy.contains(&work_dir) {
            return Err(format!("工作目录 {work_dir} 已有监督任务在运行"));
        }
        busy.push(work_dir.clone());
    }

    let mut child = match supervise_runner::spawn_supervise(&request) {
        Ok(c) => c,
        Err(e) => {
            state
                .busy_workdirs
                .lock()
                .unwrap()
                .retain(|w| w != &work_dir);
            return Err(e);
        }
    };
    let task_id = format!("task-{}", TASK_COUNTER.fetch_add(1, Ordering::Relaxed));
    let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
    // stderr 必须有人消费：写满管道缓冲会挂死子进程（busy_workdirs 永不清理）。
    // 逐行并入 supervise-log（带 [stderr] 前缀），诊断信息直接进 UI 日志流。
    let stderr = child.stderr.take();
    state.running.lock().unwrap().insert(task_id.clone(), child);

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

    if let Some(stderr) = stderr {
        let app3 = app.clone();
        let task_id3 = task_id.clone();
        supervise_runner::drain_stderr(stderr, move |line| {
            let _ = app3.emit(
                "supervise-log",
                serde_json::json!({ "taskId": task_id3, "line": format!("[stderr] {line}") }),
            );
        });
    }

    Ok(task_id)
}

/// 取消运行中的监督任务（kill 整个进程树：pwsh + claude/codex/node 子进程）
#[tauri::command]
async fn cancel_supervise(app: AppHandle, task_id: String) -> Result<(), String> {
    let state = app.state::<SuperviseState>();
    let mut running = state.running.lock().unwrap();
    if let Some((pid, mut child)) = running.remove(&task_id).map(|c| (c.id(), c)) {
        // 先杀整棵进程树再杀主进程：父进程先死后 taskkill /T 枚举不到子树，
        // claude/codex 子孙会变孤儿继续跑（与 stop_terminal 同款顺序）
        if let Err(error) = kill_process_tree(Some(pid)) {
            eprintln!("[supervise] 终止监督进程树（PID {pid}）失败: {error}");
        }
        let _ = child.kill();
        let _ = child.wait();
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
    Ok(mcp_checker::check_mcp(&mcp_checker::default_config_path()))
}

/// MCP 一键修复（备份 + 最小侵入插入缺失项 + 原子写）
#[tauri::command]
async fn fix_mcp() -> Result<mcp_checker::FixResult, String> {
    Ok(mcp_checker::fix_mcp(&mcp_checker::default_config_path()))
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
    let entries = session_proxy::get_transcript(&file, None, None).map_err(|e| e.to_string())?;
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
        .manage(TerminalState {
            sessions: Mutex::new(HashMap::new()),
        })
        .setup(|app| {
            // 注入打包资源里的 server.js / supervise.ps1 路径（发布/开发均来自
            // BaseDirectory::Resource，即 exe 同级目录；替代旧的 mcp-lab 绝对路径）
            let server_js = app.path().resolve(
                "resources/agent-sessions-mcp/server.js",
                tauri::path::BaseDirectory::Resource,
            )?;
            session_proxy::set_server_js(server_js.clone());
            // mcp_checker 修复注册时写入同一个内置副本路径
            mcp_checker::set_server_js(server_js);
            let supervise_ps1 = app.path().resolve(
                "resources/supervise-loop-script/supervise.ps1",
                tauri::path::BaseDirectory::Resource,
            )?;
            supervise_runner::set_supervise_script(supervise_ps1);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            get_transcript,
            search_sessions,
            start_terminal,
            write_terminal,
            resize_terminal,
            stop_terminal,
            run_supervise,
            cancel_supervise,
            read_review_artifacts,
            check_mcp,
            fix_mcp,
            export_transcript_md,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                window.state::<TerminalState>().stop_all();
                window.state::<SuperviseState>().stop_all();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
