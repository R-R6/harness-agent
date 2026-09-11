// 会话数据代理：内置 agent-sessions-mcp (server.js)，资源路径由 setup 注入（独立 crate：crates/session_proxy）
use session_proxy::{SessionInfo, TranscriptEntry};
use supervise_engine::{
    hook::{ensure_stop_hook, remove_stop_hook}, CodexReviewer, EngineOptions, MarkerSource,
    MockReviewer, PaneIo, Reviewer, Verdict,
};
use supervise_runner::{ReviewArtifact, SuperviseContinueRequest, SuperviseRequest};
use terminal_host::{
    claude_config_path, ensure_folder_trusted, kill as kill_terminal_process,
    resize as resize_terminal_pty, spawn as spawn_terminal_pty, terminal_command,
    wait as wait_terminal_process, write_input as write_terminal_input,
};

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

/// 监督任务 id 自增计数器（进程内；重启后 alloc_task_id 会越过已有 task-N）
static TASK_COUNTER: AtomicU64 = AtomicU64::new(1);
static TERMINAL_COUNTER: AtomicU64 = AtomicU64::new(1);

// ---------------- 任务注册表（阶段 B：多任务状态管理） ----------------

/// 任务状态（复用 EngineStatus 语义；ps1 无头模式由退出码推导）
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Running,
    Accepted,
    Rejected,
    Cancelled,
    Aborted,
}

/// 任务类型
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum TaskKind {
    Ps1,
    Engine,
}

/// 任务注册表条目（持久化到 app_data_dir/supervise/tasks.json，重启保留）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TaskInfo {
    id: String,
    work_dir: String,
    task: String,
    kind: TaskKind,
    status: TaskStatus,
    rounds: i64,
    last_reason: String,
    log: Vec<String>,
    /// 模拟模式（续跑「再来一轮」沿用同一审查方式）
    #[serde(default)]
    mock: bool,
    started_at_ms: u64,
}

/// 毫秒级时间戳（用于 started_at）
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// ps1 无头模式的退出码 → 任务终态映射（与引擎模式由 EngineStatus 直推不同）：
/// 0=通过、1=驳回、其它=中止、None=取消（进程被 kill 掉时 wait 返回 None）
fn ps1_exit_to_status(code: Option<i64>) -> TaskStatus {
    match code {
        Some(0) => TaskStatus::Accepted,
        Some(1) => TaskStatus::Rejected,
        Some(_) => TaskStatus::Aborted,
        None => TaskStatus::Cancelled,
    }
}

// ---------------- 监督进程状态（并发锁 + 进程表） ----------------

struct SuperviseState {
    /// task_id → 运行中的子进程（ps1 无头模式，用于取消）
    running: Mutex<HashMap<String, std::process::Child>>,
    /// 终端驱动引擎：同一 Claude PTY session 互斥（同目录多进程允许，无上限）
    engine_busy_sessions: Mutex<HashSet<String>>,
    /// task_id → 终端驱动引擎（阶段 2）的取消标志
    engine_cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// 任务注册表（含历史终态；应用退出即清，重启后历史消失属预期）
    tasks: Mutex<HashMap<String, TaskInfo>>,
    /// 实际安装过 Stop hook 的引擎数（mock 不计入，避免卡住卸载）
    engine_hook_refs: Mutex<u32>,
}

fn engine_session_available(busy: &HashSet<String>, session_id: &str) -> bool {
    !busy.contains(session_id)
}

fn release_engine_session(state: &SuperviseState, session_id: &str) {
    if let Ok(mut busy) = state.engine_busy_sessions.lock() {
        busy.remove(session_id);
    }
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
        // 引擎线程：置取消标志（注入/等待循环每 500ms 检查一次即退出）
        if let Ok(flags) = self.engine_cancels.lock() {
            for flag in flags.values() {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }
}

// ---------------- 任务注册表持久化（文件系统） ----------------

/// 每任务日志上限（截断旧行，避免 tasks.json 无限膨胀）
const TASK_LOG_MAX_LINES: usize = 500;

fn parse_task_num(id: &str) -> Option<u64> {
    id.strip_prefix("task-")?.parse().ok()
}

fn max_task_num_in_dir(dir: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .filter_map(|e| parse_task_num(&e.file_name().to_string_lossy()))
        .max()
        .unwrap_or(0)
}

/// 已占用的最大 task-N：注册表全部 id + 本目录及已知工作目录的 `.supervise/tasks/`。
/// 产物按 `{work_dir}/.supervise/tasks/<id>/` 隔离；id 必须全局唯一，否则
/// HashMap / 重启后的计数器会把新任务写进第一个空间的 `task-1`（真实事故）。
fn known_task_num_floor(existing: &HashMap<String, TaskInfo>, work_dir: &str) -> u64 {
    let mut max = existing
        .keys()
        .filter_map(|id| parse_task_num(id))
        .max()
        .unwrap_or(0);
    let mut dirs: HashSet<String> = HashSet::new();
    if !work_dir.trim().is_empty() {
        dirs.insert(work_dir.trim().to_string());
    }
    for t in existing.values() {
        if !t.work_dir.trim().is_empty() {
            dirs.insert(t.work_dir.clone());
        }
    }
    for d in dirs {
        max = max.max(max_task_num_in_dir(
            &std::path::Path::new(&d).join(".supervise").join("tasks"),
        ));
    }
    max
}

fn alloc_task_id(existing: &HashMap<String, TaskInfo>, work_dir: &str) -> String {
    let floor = known_task_num_floor(existing, work_dir);
    loop {
        let n = TASK_COUNTER.fetch_add(1, Ordering::Relaxed);
        if n > floor {
            return format!("task-{n}");
        }
    }
}

/// 任务注册表落盘路径：{app_data_dir}/supervise/tasks.json
fn tasks_file_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法定位应用数据目录: {e}"))?;
    Ok(dir.join("supervise").join("tasks.json"))
}

/// 全量序列化写盘（临时文件 + rename 原子写）。锁中毒/路径不可用/序列化失败
/// 均静默降级——持久化是增强能力，不能反过来崩掉主流程。
fn persist_tasks(app: &AppHandle) {
    let tasks: Vec<TaskInfo> = match app.state::<SuperviseState>().tasks.lock() {
        Ok(t) => t.values().cloned().collect(),
        Err(_) => return,
    };
    let path = match tasks_file_path(app) {
        Ok(p) => p,
        Err(_) => return,
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let data = match serde_json::to_string_pretty(&tasks) {
        Ok(d) => d,
        Err(_) => return,
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, data).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// 启动时加载持久化任务。重启后已无进程在跑：running 一律标记 aborted 并回写。
fn load_tasks(app: &AppHandle) {
    let path = match tasks_file_path(app) {
        Ok(p) => p,
        Err(_) => return,
    };
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return, // 首次启动无文件
    };
    let mut tasks: Vec<TaskInfo> = match serde_json::from_str(&data) {
        Ok(t) => t,
        Err(_) => return,
    };
    for t in tasks.iter_mut() {
        if t.status == TaskStatus::Running {
            t.status = TaskStatus::Aborted;
        }
    }
    if let Ok(mut map) = app.state::<SuperviseState>().tasks.lock() {
        for t in tasks {
            map.insert(t.id.clone(), t);
        }
        let floor = known_task_num_floor(&map, "");
        let next = floor.saturating_add(1);
        let cur = TASK_COUNTER.load(Ordering::Relaxed);
        if cur <= floor {
            TASK_COUNTER.store(next, Ordering::Relaxed);
        }
    }
    persist_tasks(app);
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
    agent: String,
    work_dir: String,
    writer: std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
    master: std::sync::Arc<std::sync::Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    child: std::sync::Arc<std::sync::Mutex<Box<dyn portable_pty::Child + Send>>>,
    pid: Option<u32>,
    recent_output: Arc<Mutex<String>>,
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
    recent_output: &Arc<Mutex<String>>,
) {
    loop {
        match std::str::from_utf8(pending) {
            Ok(data) => {
                if !data.is_empty() {
                    append_recent_output(recent_output, data);
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
                    append_recent_output(recent_output, data);
                    let _ = app.emit(
                        "terminal-output",
                        serde_json::json!({ "sessionId": session_id, "data": data }),
                    );
                    pending.drain(..valid);
                    continue;
                }
                if let Some(invalid_len) = error.error_len() {
                    let replacement = String::from_utf8_lossy(&pending[..invalid_len]).to_string();
                    append_recent_output(recent_output, &replacement);
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

fn append_recent_output(buf: &Arc<Mutex<String>>, data: &str) {
    const MAX: usize = 24_000;
    let Ok(mut s) = buf.lock() else {
        return;
    };
    s.push_str(data);
    if s.len() > MAX {
        // 截断点必须落在 UTF-8 字符边界（TUI 中文输出是多字节）
        let mut excess = s.len() - MAX;
        while !s.is_char_boundary(excess) {
            excess += 1;
        }
        s.drain(..excess);
    }
}

/// taskkill /T /F 杀掉整棵进程树（含 PTY 主进程）。
/// 必须在父进程还活着时调用——父进程先死后 taskkill 枚举不到子树，
/// 子孙进程会变成孤儿继续存活。目标进程已退出（taskkill 报"没有找到进程"）视为成功。
fn kill_process_tree(pid: Option<u32>) -> Result<(), String> {
    let Some(pid) = pid else { return Ok(()) };
    #[cfg(windows)]
    {
        let mut command = std::process::Command::new("taskkill");
        path_util::no_console_window(&mut command);
        let output = command
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

/// Agent 注册表状态（多 Agent 监督）：每个 CLI 的角色能力与本机安装/会话探测
#[tauri::command]
fn agent_catalog() -> Vec<agent_registry::AgentStatus> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    agent_registry::status_all(&home)
}

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
    if request.agent == "claude" {
        if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
            let cfg = claude_config_path(std::path::Path::new(&home));
            if let Err(error) = ensure_folder_trusted(&cfg, &request.work_dir) {
                // 预信任失败不阻断启动（Claude 会弹信任框，用户手动选 Yes 即可），
                // 但必须让用户看见，而不是只进控制台日志
                let _ = app.emit(
                    "terminal-notice",
                    serde_json::json!({
                        "agent": request.agent,
                        "workDir": request.work_dir,
                        "message": format!("工作目录预信任失败（{error}）；Claude 可能弹信任对话框，请手动选 Yes"),
                    }),
                );
            }
        }
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
        agent: request.agent.clone(),
        work_dir: request.work_dir.clone(),
        writer: spawned.writer,
        master: spawned.master,
        child: spawned.child,
        pid: spawned.pid,
        recent_output: Arc::new(Mutex::new(String::new())),
    };
    let recent_output = process.recent_output.clone();
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
                    emit_terminal_output(&app2, &id2, &mut pending_utf8, &recent_output);
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
            append_recent_output(&recent_output, &data);
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
    // 无头模式启动前同样预检端点可用
    supervise_runner::preflight_claude(&work_dir)?;
    // 无头模式：同目录可并发（产物按 task_id 隔离）。先分配 id 再 spawn，
    // 以便 SUPERVISE_TASK_ID 进入子进程环境。id 必须越过注册表/磁盘已有编号。
    let task_id = {
        let tasks = state.tasks.lock().map_err(|_| "任务状态锁已损坏".to_string())?;
        alloc_task_id(&tasks, &work_dir)
    };

    let mut child = match supervise_runner::spawn_supervise(&request, Some(&task_id)) {
        Ok(c) => c,
        Err(e) => return Err(e),
    };
    let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
    // stderr 必须有人消费：写满管道缓冲会挂死子进程（任务永不收尾）。
    // 逐行并入 supervise-log（带 [stderr] 前缀），诊断信息直接进 UI 日志流。
    let stderr = child.stderr.take();
    state.running.lock().unwrap().insert(task_id.clone(), child);
    // 登记任务注册表（Running 态）
    state.tasks.lock().unwrap().insert(task_id.clone(), TaskInfo {
        id: task_id.clone(),
        work_dir: work_dir.clone(),
        task: request.task.clone(),
        kind: TaskKind::Ps1,
        status: TaskStatus::Running,
        rounds: 0,
        last_reason: String::new(),
        log: Vec::new(),
        mock: request.mock,
        started_at_ms: now_ms(),
    });
    persist_tasks(&app);

    // 后台线程读 stdout → 逐行 emit 到前端；进程结束后清理 State 并 emit done
    let app2 = app.clone();
    let task_id2 = task_id.clone();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(l) = line {
                let _ = app2.emit(
                    "supervise-log",
                    serde_json::json!({ "taskId": task_id2, "line": l.clone() }),
                );
                // 落盘用：逐行进内存，终态时统一写 tasks.json
                if let Ok(mut tasks) = app2.state::<SuperviseState>().tasks.lock() {
                    if let Some(t) = tasks.get_mut(&task_id2) {
                        if t.log.len() < TASK_LOG_MAX_LINES {
                            t.log.push(l);
                        }
                    }
                }
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
        // 更新任务注册表终态
        {
            let mut tasks = running.tasks.lock().unwrap();
            if let Some(t) = tasks.get_mut(&task_id2) {
                t.status = ps1_exit_to_status(exit_code);
            }
        }
        persist_tasks(&app2);
        let _ = app2.emit(
            "supervise-done",
            serde_json::json!({ "taskId": task_id2, "exitCode": exit_code }),
        );
    });

    if let Some(stderr) = stderr {
        let app3 = app.clone();
        let task_id3 = task_id.clone();
        supervise_runner::drain_stderr(stderr, move |line| {
            let text = format!("[stderr] {line}");
            let _ = app3.emit(
                "supervise-log",
                serde_json::json!({ "taskId": task_id3, "line": text.clone() }),
            );
            if let Ok(mut tasks) = app3.state::<SuperviseState>().tasks.lock() {
                if let Some(t) = tasks.get_mut(&task_id3) {
                    if t.log.len() < TASK_LOG_MAX_LINES {
                        t.log.push(text);
                    }
                }
            }
        });
    }

    Ok(task_id)
}

/// 取消运行中的监督任务（ps1 无头模式杀进程树；终端驱动引擎置取消标志）
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
    drop(running);
    if let Some(flag) = state.engine_cancels.lock().unwrap().get(&task_id) {
        flag.store(true, Ordering::Relaxed);
        return Ok(());
    }
    Err("任务不存在或已结束".into())
}

// ---------------- 终端驱动监督引擎（阶段 2） ----------------

/// Level → 轮数推导（与 supervise.ps1 Get-LevelDefaults 同语义）。
/// 模型不在这里推导：审查模型直接取 request.model（默认空 = 跟随 codex
/// 配置的默认模型）——硬编码模型名会随中转服务分组变更而 404
/// （真实事故：gpt-5.6-luna 不被当前账号支持）
fn level_rounds(level: Option<&str>, max_rounds: Option<i64>) -> i64 {
    let base = match level {
        Some("L0") => 1,
        Some("L2") => 5,
        _ => 3, // L1 与未知值：默认 3 轮
    };
    max_rounds.filter(|m| *m > 0).unwrap_or(base)
}

/// 引擎与终端 pane 的桥：注入走 pane 的 PTY writer；绑定校验查会话表
struct TerminalPaneAdapter {
    app: AppHandle,
    session_id: String,
}

impl PaneIo for TerminalPaneAdapter {
    fn write(&self, data: &str) -> Result<(), String> {
        let state = self.app.state::<TerminalState>();
        let sessions = state.sessions.lock().map_err(|_| "终端状态锁已损坏")?;
        let process = sessions
            .get(&self.session_id)
            .ok_or("终端会话已退出")?;
        write_terminal_input(&process.writer, data.as_bytes())
    }

    fn current_work_dir(&self) -> Option<String> {
        let state = self.app.state::<TerminalState>();
        sessions_dir_of(&state, &self.session_id)
    }

    fn recent_output(&self) -> String {
        let state = self.app.state::<TerminalState>();
        state
            .sessions
            .lock()
            .ok()
            .and_then(|sessions| {
                sessions
                    .get(&self.session_id)
                    .and_then(|p| p.recent_output.lock().ok().map(|g| g.clone()))
            })
            .unwrap_or_default()
    }
}

fn sessions_dir_of(state: &TerminalState, session_id: &str) -> Option<String> {
    state
        .sessions
        .lock()
        .ok()?
        .get(session_id)
        .map(|p| p.work_dir.clone())
}

/// 终端驱动监督：任务注入运行中的 Claude 终端 pane，干活全程可见、人可插手；
/// 轮次完成靠 Stop hook marker（启动时幂等安装），审查走无头 codex exec。
#[tauri::command]
async fn run_supervise_terminal(
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

    // 启动前预检 Claude CLI 端点可用（403/未登录等在此拦下，不烧任务轮次）
    supervise_runner::preflight_claude(&work_dir)?;

    // 一任务一 Claude PTY：优先用请求里的 terminal_session_id；否则取该目录
    // 上尚未被引擎占用的 Claude pane（兼容旧前端）。解析与占用同一把锁，防竞态。
    let session_id = {
        let term = app.state::<TerminalState>();
        let sessions = term.sessions.lock().map_err(|_| "终端状态锁已损坏")?;
        let mut busy = state.engine_busy_sessions.lock().unwrap();
        let id = if let Some(want) = request
            .terminal_session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let proc = sessions.get(want).ok_or_else(|| {
                format!("指定的终端会话不存在或已退出：{want}")
            })?;
            if proc.agent != "claude" {
                return Err(format!("指定会话不是 Claude 终端：{want}"));
            }
            if normalize_path(&proc.work_dir) != work_dir {
                return Err(format!(
                    "指定 Claude 会话目录与任务目录不符（会话 {}，任务 {}）",
                    proc.work_dir, request.work_dir
                ));
            }
            if !engine_session_available(&busy, want) {
                return Err("该 Claude 终端已有监督任务在运行（同一会话不能并行注入）".into());
            }
            want.to_string()
        } else {
            sessions
                .iter()
                .find(|(id, p)| {
                    p.agent == "claude"
                        && normalize_path(&p.work_dir) == work_dir
                        && engine_session_available(&busy, id)
                })
                .map(|(id, _)| id.clone())
                .ok_or_else(|| {
                    "未找到可用的 Claude 终端（请先启动 Claude，或传入 terminal_session_id）"
                        .to_string()
                })?
        };
        busy.insert(id.clone());
        id
    };

    let rounds = level_rounds(request.level.as_deref(), request.max_rounds);
    let model = request.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
    let task_id = {
        let tasks = state.tasks.lock().map_err(|_| "任务状态锁已损坏".to_string())?;
        alloc_task_id(&tasks, &work_dir)
    };
    // 令牌需单次运行唯一：task_id 在应用重启后从头计数，若令牌只带 task_id，
    // 前一次运行的陈旧会话会与新任务同令牌、被预钉误命中（真实事故：同目录
    // 二次运行 task-2，注入实际落进新会话，引擎却钉死旧会话，审查/确认全对着错文件）
    let started_at = now_ms();

    // 产物按 task 隔离；Stop marker 仍共享根目录（引擎靠 task_token 预钉 session）
    let supervise_dir = std::path::Path::new(&request.work_dir).join(".supervise");
    let artifacts_dir = supervise_dir.join("tasks").join(&task_id);
    if let Err(e) = std::fs::create_dir_all(&artifacts_dir) {
        release_engine_session(&state, &session_id);
        return Err(format!("创建任务产物目录失败: {e}"));
    }
    let marker_file = supervise_dir.join("stop-markers.jsonl");
    if let Err(e) = std::fs::create_dir_all(&supervise_dir) {
        release_engine_session(&state, &session_id);
        return Err(format!("创建 .supervise 失败: {e}"));
    }
    let home = match std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        Ok(h) => h,
        Err(_) => {
            release_engine_session(&state, &session_id);
            return Err("无法定位用户主目录（USERPROFILE/HOME 均缺失）".into());
        }
    };
    let settings = std::path::Path::new(&home).join(".claude").join("settings.json");
    let hook_installed = !request.mock;
    if hook_installed {
        if let Err(e) = ensure_stop_hook(&settings, &marker_file) {
            release_engine_session(&state, &session_id);
            return Err(format!("安装 Stop hook 失败（可重试或改用无头模式）: {e}"));
        }
        *state.engine_hook_refs.lock().unwrap() += 1;
    }

    let cancel = Arc::new(AtomicBool::new(false));
    state
        .engine_cancels
        .lock()
        .unwrap()
        .insert(task_id.clone(), cancel.clone());
    // 登记任务注册表（Running 态）
    state.tasks.lock().unwrap().insert(task_id.clone(), TaskInfo {
        id: task_id.clone(),
        work_dir: work_dir.clone(),
        task: request.task.clone(),
        kind: TaskKind::Engine,
        status: TaskStatus::Running,
        rounds: 0,
        last_reason: String::new(),
        log: Vec::new(),
        mock: request.mock,
        started_at_ms: started_at,
    });
    persist_tasks(&app);

    let opts = EngineOptions {
        task: request.task.clone(),
        work_dir: request.work_dir.clone(),
        max_rounds: rounds,
        artifacts_dir: Some(artifacts_dir),
        task_token: Some(format!("{task_id}:{started_at}")),
        reviewer_label: match (request.mock, model) {
            (true, _) => "mock".to_string(),
            (false, Some(m)) => m.to_string(),
            (false, None) => "codex 默认模型".to_string(),
        },
        ..Default::default()
    };
    let reviewer: Arc<dyn Reviewer> = if request.mock {
        // mock：第 1 轮模拟返工意见、第 2 轮通过（无 CLI 环境也能演示全链路）
        Arc::new(MockReviewer::scripted(vec![
            Ok(Verdict { pass: false, reason: "（模拟）缺少输入校验，请补充。".into() }),
            Ok(Verdict { pass: true, reason: "（模拟）校验已补齐，验收通过。".into() }),
        ]))
    } else {
        Arc::new(CodexReviewer::new(model, &request.task))    };

    spawn_engine_thread(
        &app,
        task_id.clone(),
        session_id,
        home,
        hook_installed,
        settings,
        marker_file,
        opts,
        reviewer,
        cancel,
    );

    Ok(task_id)
}

/// 引擎线程通用接线：标记源、pane 适配器、日志追加（emit + TaskInfo.log）、
/// panic 兜底、收尾清理（engine_cancels/busy/hook 引用）、终态写回、done 事件。
/// run_supervise_terminal 与 continue_supervise_terminal（「再来一轮」）共用。
#[allow(clippy::too_many_arguments)]
fn spawn_engine_thread(
    app: &AppHandle,
    task_id: String,
    session_id: String,
    home: String,
    hook_installed: bool,
    settings: std::path::PathBuf,
    marker_file: std::path::PathBuf,
    opts: supervise_engine::EngineOptions,
    reviewer: Arc<dyn supervise_engine::Reviewer>,
    cancel: Arc<AtomicBool>,
) {
    let app2 = app.clone();
    let task_id2 = task_id;
    let session_id2 = session_id.clone();
    let settings2 = settings;
    let marker_file2 = marker_file.clone();
    std::thread::spawn(move || {
        let markers = MarkerSource::new(marker_file);
        let projects_root = std::path::PathBuf::from(&home).join(".claude").join("projects");
        let pane = Arc::new(TerminalPaneAdapter {
            app: app2.clone(),
            session_id,
        });
        let app3 = app2.clone();
        let task_id3 = task_id2.clone();
        let on_log: supervise_engine::OnLog =
            Arc::new(move |line: &str| {
                let text = line.to_string();
                let _ = app3.emit(
                    "supervise-log",
                    serde_json::json!({ "taskId": task_id3, "line": text.clone() }),
                );
                if let Ok(mut tasks) = app3.state::<SuperviseState>().tasks.lock() {
                    if let Some(t) = tasks.get_mut(&task_id3) {
                        if t.log.len() < TASK_LOG_MAX_LINES {
                            t.log.push(text);
                        }
                    }
                }
            });
        // 引擎 panic（锁中毒/审查器内部异常）也必须走到收尾——否则目录永久
        // 占用、engine_cancels 泄漏、前端永远停在"取消任务"状态
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                supervise_engine::run(&opts, pane, reviewer, &markers, &projects_root, &cancel, &on_log)
            }))
            .unwrap_or_else(|panic| {
                let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "未知异常".to_string()
                };
                supervise_engine::EngineOutcome {
                    status: supervise_engine::EngineStatus::Aborted(format!("引擎线程异常: {msg}")),
                    rounds: 0,
                    last_reason: msg,
                }
            });

        // 收尾：清登记 →（无其他引擎时）卸载 hook → emit done。
        // Stop hook 指向本项目 marker，遗留会让用户所有 Claude 会话每次
        // Stop 都白跑一次 powershell，多项目还会累积死条目
        let app_state = app2.state::<SuperviseState>();
        {
            let mut flags = app_state.engine_cancels.lock().unwrap();
            flags.remove(&task_id2);
        }
        app_state
            .engine_busy_sessions
            .lock()
            .unwrap()
            .remove(&session_id2);
        let remaining_hooks = if hook_installed {
            let mut n = app_state.engine_hook_refs.lock().unwrap();
            *n = n.saturating_sub(1);
            *n
        } else {
            *app_state.engine_hook_refs.lock().unwrap()
        };
        if hook_installed && remaining_hooks == 0 {
            if let Err(e) = remove_stop_hook(&settings2, &marker_file2) {
                eprintln!("[supervise] 卸载 Stop hook 失败（不影响任务结果）: {e}");
            }
        }
        let code = match outcome.status {
            supervise_engine::EngineStatus::Accepted
            | supervise_engine::EngineStatus::Cancelled => 0,
            supervise_engine::EngineStatus::Rejected => 1,
            supervise_engine::EngineStatus::Aborted(_) => 2,
        };
        // 更新任务注册表终态
        {
            let mut tasks = app_state.tasks.lock().unwrap();
            if let Some(t) = tasks.get_mut(&task_id2) {
                t.status = match outcome.status {
                    supervise_engine::EngineStatus::Accepted => TaskStatus::Accepted,
                    supervise_engine::EngineStatus::Rejected => TaskStatus::Rejected,
                    supervise_engine::EngineStatus::Cancelled => TaskStatus::Cancelled,
                    supervise_engine::EngineStatus::Aborted(_) => TaskStatus::Aborted,
                };
                t.rounds = outcome.rounds;
                t.last_reason = outcome.last_reason.clone();
            }
        }
        persist_tasks(&app2);
        let _ = app2.emit(
            "supervise-done",
            serde_json::json!({ "taskId": task_id2, "exitCode": code, "reason": outcome.last_reason }),
        );
    });
}

/// 「再来一轮」状态门控（纯函数，可测）：
/// Rejected → 注入上轮审查意见返工；Aborted/Cancelled → 以原任务重启
/// （真机：中止常发生在"输入栏未就绪/端点不可用"，任务正文从未落地，
/// 用户此前只能删任务重来）。Running/Accepted 不允许续跑。
fn validate_continue_status(status: TaskStatus, last_reason: &str) -> Result<bool, String> {
    match status {
        TaskStatus::Rejected => {
            if last_reason.trim().is_empty() {
                Err("任务没有上轮审查意见，无法再来一轮".into())
            } else {
                Ok(true)
            }
        }
        TaskStatus::Aborted | TaskStatus::Cancelled => Ok(false),
        TaskStatus::Running => Err("任务仍在运行中，无法再来一轮".into()),
        TaskStatus::Accepted => Err("任务已通过验收，无需再来一轮".into()),
    }
}

/// 「再来一轮」：rejected 任务复用原 Claude 会话追加一轮完整闭环。
/// 注入上轮审查意见 → Claude 同会话落地 → Stop hook → codex 审查 → verdict。
/// 复用原 artifacts/marker/令牌，轮次从原 rounds 继续（第 N+1 轮）。
#[tauri::command]
async fn continue_supervise_terminal(
    app: AppHandle,
    state: State<'_, SuperviseState>,
    request: SuperviseContinueRequest,
) -> Result<String, String> {
    // 1) 校验：任务存在、engine 类型、可续跑终态（未通过=返工；中止/取消=重启）
    let (task, work_dir, last_reason, started_at, mock, is_rework) = {
        let tasks = state.tasks.lock().map_err(|_| "任务状态锁已损坏".to_string())?;
        let t = tasks
            .get(&request.task_id)
            .ok_or_else(|| format!("任务不存在: {}", request.task_id))?;
        if t.kind != TaskKind::Engine {
            return Err("仅驱动 Claude 终端的任务支持「再来一轮」".into());
        }
        let is_rework = validate_continue_status(t.status, &t.last_reason)?;
        (
            t.clone(),
            t.work_dir.clone(),
            t.last_reason.clone(),
            t.started_at_ms,
            t.mock,
            is_rework,
        )
    };
    let work_dir = normalize_path(&work_dir);
    if work_dir.is_empty() || !std::path::Path::new(&work_dir).is_dir() {
        return Err(format!("工作目录不存在: {work_dir}"));
    }
    // 续跑前同样预检端点可用，避免白装 hook 再中止
    supervise_runner::preflight_claude(&work_dir)?;

    // 2) 复用原 Claude pane：FAIL 后引擎已释放占用，按目录直接命中空闲 pane
    let session_id = {
        let term = app.state::<TerminalState>();
        let sessions = term.sessions.lock().map_err(|_| "终端状态锁已损坏")?;
        let mut busy = state.engine_busy_sessions.lock().unwrap();
        let id = sessions
            .iter()
            .find(|(id, p)| {
                p.agent == "claude"
                    && normalize_path(&p.work_dir) == work_dir
                    && engine_session_available(&busy, id)
            })
            .map(|(id, _)| id.clone())
            .ok_or_else(|| {
                "未找到原 Claude 终端（可能已被关闭）。请重开 Claude 后手动处理最后意见。"
                    .to_string()
            })?;
        busy.insert(id.clone());
        id
    };

    // 3) 复用产物/marker 目录（与原始任务同路径）
    let supervise_dir = std::path::Path::new(&work_dir).join(".supervise");
    let artifacts_dir = supervise_dir.join("tasks").join(&request.task_id);
    if let Err(e) = std::fs::create_dir_all(&artifacts_dir) {
        release_engine_session(&state, &session_id);
        return Err(format!("创建任务产物目录失败: {e}"));
    }
    let marker_file = supervise_dir.join("stop-markers.jsonl");
    if let Err(e) = std::fs::create_dir_all(&supervise_dir) {
        release_engine_session(&state, &session_id);
        return Err(format!("创建 .supervise 失败: {e}"));
    }

    // 4) Stop hook 重装（真实模式；原任务 FAIL 后已卸载）
    let home = match std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        Ok(h) => h,
        Err(_) => {
            release_engine_session(&state, &session_id);
            return Err("无法定位用户主目录（USERPROFILE/HOME 均缺失）".into());
        }
    };
    let settings = std::path::Path::new(&home).join(".claude").join("settings.json");
    let hook_installed = !mock;
    if hook_installed {
        if let Err(e) = ensure_stop_hook(&settings, &marker_file) {
            release_engine_session(&state, &session_id);
            return Err(format!("安装 Stop hook 失败（可重试）: {e}"));
        }
        *state.engine_hook_refs.lock().unwrap() += 1;
    }

    // 5) 状态 → running，追加续跑日志
    let extra = 1i64;
    let cancel = Arc::new(AtomicBool::new(false));
    state
        .engine_cancels
        .lock()
        .unwrap()
        .insert(request.task_id.clone(), cancel.clone());
    {
        let mut tasks = state.tasks.lock().unwrap();
        if let Some(t) = tasks.get_mut(&request.task_id) {
            t.status = TaskStatus::Running;
        }
    }
    persist_tasks(&app);
    {
        let line = format!(
            "[ENGINE] 监督引擎续跑：再来 {extra} 轮（第 {} 轮起），复用原 Claude 会话…",
            task.rounds + 1
        );
        if let Ok(mut tasks) = state.tasks.lock() {
            if let Some(t) = tasks.get_mut(&request.task_id) {
                if t.log.len() < TASK_LOG_MAX_LINES {
                    t.log.push(line);
                }
            }
        }
        persist_tasks(&app);
    }

    // 6) opts + reviewer（沿用原令牌/审查方式；未通过注入审查意见返工，
    // 中止/取消以原任务正文重启——中止时 last_reason 是失败原因，不是审查意见）
    let rework_text = if is_rework {
        format!("上一轮审查未通过，请按要求返工：{last_reason}")
    } else {
        task.task.clone()
    };
    let opts = supervise_engine::EngineOptions {
        task: rework_text.clone(),
        work_dir: work_dir.clone(),
        max_rounds: task.rounds + extra,
        starting_round: task.rounds,
        artifacts_dir: Some(artifacts_dir),
        task_token: Some(format!("{}:{started_at}", request.task_id)),
        reviewer_label: if mock { "mock".into() } else { "codex 默认模型".into() },
        ..Default::default()
    };
    let reviewer: Arc<dyn supervise_engine::Reviewer> = if mock {
        Arc::new(supervise_engine::MockReviewer::always(Ok(
            supervise_engine::Verdict {
                pass: true,
                reason: "（模拟）续跑验收通过".into(),
            },
        )))
    } else {
        Arc::new(supervise_engine::CodexReviewer::new(None, &rework_text))
    };

    // 7) 启动引擎线程（与首发共用接线）
    spawn_engine_thread(
        &app,
        request.task_id.clone(),
        session_id,
        home,
        hook_installed,
        settings,
        marker_file,
        opts,
        reviewer,
        cancel,
    );
    Ok(request.task_id)
}

/// 读取监督闭环产物（.supervise 目录；无头任务可指定 task_id 读子目录）
#[tauri::command]
async fn read_review_artifacts(
    work_dir: String,
    task_id: Option<String>,
) -> Result<Vec<ReviewArtifact>, String> {
    supervise_runner::read_artifacts(&work_dir, task_id.as_deref())
}

/// 列出全部监督任务（含历史终态；持久化到 app_data_dir，重启后仍在）
#[tauri::command]
fn list_supervise_tasks(state: State<'_, SuperviseState>) -> Vec<TaskInfo> {
    state
        .tasks
        .lock()
        .map(|tasks| {
            let mut out: Vec<TaskInfo> = tasks.values().cloned().collect();
            out.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms)); // 最新在前
            out
        })
        .unwrap_or_default()
}

/// 删除任务记录（连带删产物目录）。运行中的任务拒绝删除（需先取消）。
#[tauri::command]
fn delete_supervise_task(app: AppHandle, task_id: String) -> Result<(), String> {
    let state = app.state::<SuperviseState>();
    let removed = {
        let mut tasks = state.tasks.lock().map_err(|_| "任务注册表锁已损坏")?;
        let running = tasks
            .get(&task_id)
            .map(|t| t.status == TaskStatus::Running);
        match running {
            None => return Err("任务不存在".into()),
            Some(true) => return Err("任务正在运行，请先取消再删除".into()),
            Some(false) => tasks.remove(&task_id).map(|t| t.work_dir),
        }
    };
    // D2：删除任务即连带清理 .supervise/tasks/<id>/ 产物
    if let Some(work_dir) = removed {
        let artifacts = std::path::Path::new(&work_dir)
            .join(".supervise")
            .join("tasks")
            .join(&task_id);
        let _ = std::fs::remove_dir_all(&artifacts);
    }
    persist_tasks(&app);
    Ok(())
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
            engine_busy_sessions: Mutex::new(HashSet::new()),
            engine_hook_refs: Mutex::new(0),
            engine_cancels: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
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
            load_tasks(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            agent_catalog,
            list_sessions,
            get_transcript,
            search_sessions,
            start_terminal,
            write_terminal,
            resize_terminal,
            stop_terminal,
            run_supervise,
            cancel_supervise,
            run_supervise_terminal,
            continue_supervise_terminal,
            read_review_artifacts,
            list_supervise_tasks,
            delete_supervise_task,
            check_mcp,
            fix_mcp,
            export_transcript_md,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                window.state::<TerminalState>().stop_all();
                window.state::<SuperviseState>().stop_all();
                persist_tasks(window.app_handle());
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ---------------- 测试 ----------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps1_exit_to_status_maps_all_cases() {
        assert_eq!(ps1_exit_to_status(Some(0)), TaskStatus::Accepted);
        assert_eq!(ps1_exit_to_status(Some(1)), TaskStatus::Rejected);
        assert_eq!(ps1_exit_to_status(Some(2)), TaskStatus::Aborted);
        assert_eq!(ps1_exit_to_status(Some(137)), TaskStatus::Aborted);
        // 进程被 kill（cancel）时 wait 返回 None → 取消
        assert_eq!(ps1_exit_to_status(None), TaskStatus::Cancelled);
    }

    #[test]
    fn engine_status_maps_to_task_status() {
        use supervise_engine::EngineStatus;
        let case = |s: &EngineStatus| -> TaskStatus {
            match s {
                EngineStatus::Accepted => TaskStatus::Accepted,
                EngineStatus::Rejected => TaskStatus::Rejected,
                EngineStatus::Cancelled => TaskStatus::Cancelled,
                EngineStatus::Aborted(_) => TaskStatus::Aborted,
            }
        };
        assert_eq!(case(&EngineStatus::Accepted), TaskStatus::Accepted);
        assert_eq!(case(&EngineStatus::Rejected), TaskStatus::Rejected);
        assert_eq!(case(&EngineStatus::Cancelled), TaskStatus::Cancelled);
        assert!(matches!(
            case(&EngineStatus::Aborted("".into())),
            TaskStatus::Aborted
        ));
    }

    #[test]
    fn task_info_serializes_to_snake_case_and_roundtrips() {
        let info = TaskInfo {
            id: "task-1".into(),
            work_dir: "D:\\work".into(),
            task: "写一个计算器".into(),
            kind: TaskKind::Engine,
            status: TaskStatus::Running,
            rounds: 0,
            last_reason: String::new(),
            log: vec!["line1".into(), "line2".into()],
            mock: true,
            started_at_ms: now_ms(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["kind"], "engine");
        assert_eq!(json["status"], "running");
        assert_eq!(json["task"], "写一个计算器");
        assert_eq!(json["log"], serde_json::json!(["line1", "line2"]));
        assert_eq!(json["mock"], true);

        // 持久化依赖反序列化：round-trip 后字段保持一致
        let restored: TaskInfo = serde_json::from_value(json).unwrap();
        assert_eq!(restored.task, info.task);
        assert_eq!(restored.log, info.log);
        assert_eq!(restored.kind, info.kind);
        assert_eq!(restored.status, info.status);
    }

    #[test]
    fn engine_session_available_same_session_busy_other_free() {
        let mut busy = HashSet::new();
        assert!(engine_session_available(&busy, "terminal-1"));
        busy.insert("terminal-1".into());
        assert!(!engine_session_available(&busy, "terminal-1"));
        assert!(engine_session_available(&busy, "terminal-2"));
    }

    fn stub_task(id: &str, work_dir: &str) -> TaskInfo {
        TaskInfo {
            id: id.into(),
            work_dir: work_dir.into(),
            task: "t".into(),
            kind: TaskKind::Engine,
            status: TaskStatus::Accepted,
            rounds: 1,
            last_reason: String::new(),
            log: vec![],
            mock: false,
            started_at_ms: 1,
        }
    }

    #[test]
    fn append_recent_output_truncates_on_char_boundary() {
        let buf = Arc::new(Mutex::new(String::new()));
        // 中文多字节 + 超过 24KB：截断点若不在字符边界会 panic
        let chunk = "监督引擎".repeat(3000);
        append_recent_output(&buf, &chunk);
        let s = buf.lock().unwrap().clone();
        assert!(s.len() <= 24_000 + 12);
        assert!(s.chars().next().is_some());
    }

    #[test]
    fn continue_gate_allows_rework_and_restart() {
        // 未通过：有意见 → 返工；无意见 → 拒绝
        assert_eq!(
            validate_continue_status(TaskStatus::Rejected, "缺少输入校验"),
            Ok(true)
        );
        assert!(validate_continue_status(TaskStatus::Rejected, "  ").is_err());
        // 中止/取消 → 以原任务重启（含意见为空）
        assert_eq!(validate_continue_status(TaskStatus::Aborted, ""), Ok(false));
        assert_eq!(
            validate_continue_status(TaskStatus::Cancelled, "用户取消"),
            Ok(false)
        );
        // 运行中/已通过不允许
        assert!(validate_continue_status(TaskStatus::Running, "x").is_err());
        assert!(validate_continue_status(TaskStatus::Accepted, "x").is_err());
    }

    #[test]
    fn parse_task_num_only_task_prefix() {
        assert_eq!(parse_task_num("task-1"), Some(1));
        assert_eq!(parse_task_num("task-12"), Some(12));
        assert_eq!(parse_task_num("task-x"), None);
        assert_eq!(parse_task_num("other-1"), None);
    }

    #[test]
    fn known_floor_uses_registry_and_does_not_share_across_dirs_on_disk() {
        let tmp = std::env::temp_dir().join(format!(
            "ha-taskid-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let ws_a = tmp.join("ws-a");
        let ws_b = tmp.join("ws-b");
        std::fs::create_dir_all(ws_a.join(".supervise").join("tasks").join("task-3")).unwrap();
        std::fs::create_dir_all(ws_b.join(".supervise").join("tasks").join("task-1")).unwrap();

        let mut map = HashMap::new();
        map.insert(
            "task-3".into(),
            stub_task("task-3", &ws_a.to_string_lossy()),
        );
        // 注册表最大是 3，b 盘上的 task-1 不应把全局编号打回 1
        assert_eq!(known_task_num_floor(&map, &ws_b.to_string_lossy()), 3);

        let id = alloc_task_id(&map, &ws_b.to_string_lossy());
        let n = parse_task_num(&id).expect("alloc 应返回 task-N");
        assert!(n > 3, "新空间也应从全局已占用编号继续: {id}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn artifacts_live_under_the_task_work_dir_not_a_shared_root() {
        let tmp = std::env::temp_dir().join(format!(
            "ha-art-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let ws_a = tmp.join("first-ws");
        let ws_b = tmp.join("second-ws");
        let a_dir = supervise_runner::resolve_artifact_dir(&ws_a.to_string_lossy(), Some("task-1"));
        let b_dir = supervise_runner::resolve_artifact_dir(&ws_b.to_string_lossy(), Some("task-1"));
        assert!(a_dir.starts_with(&ws_a), "{}", a_dir.display());
        assert!(b_dir.starts_with(&ws_b), "{}", b_dir.display());
        assert_ne!(a_dir, b_dir);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
