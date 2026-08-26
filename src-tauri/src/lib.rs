// 会话数据代理：内置 agent-sessions-mcp (server.js)，资源路径由 setup 注入（独立 crate：crates/session_proxy）
use session_proxy::{SessionInfo, TranscriptEntry};
use supervise_engine::{
    hook::{ensure_stop_hook, remove_stop_hook}, CodexReviewer, EngineOptions, MarkerSource,
    MockReviewer, PaneIo, Reviewer, Verdict,
};
use supervise_runner::{ReviewArtifact, SuperviseRequest};
use terminal_host::{kill as kill_terminal_process, resize as resize_terminal_pty, spawn as spawn_terminal_pty, terminal_command, wait as wait_terminal_process, write_input as write_terminal_input};

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

/// 监督任务 id 自增计数器（避免毫秒时间戳碰撞）
static TASK_COUNTER: AtomicU64 = AtomicU64::new(1);
static TERMINAL_COUNTER: AtomicU64 = AtomicU64::new(1);

// ---------------- 任务注册表（阶段 B：多任务状态管理） ----------------

/// 任务状态（复用 EngineStatus 语义；ps1 无头模式由退出码推导）
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Running,
    Accepted,
    Rejected,
    Cancelled,
    Aborted,
}

/// 任务类型
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum TaskKind {
    Ps1,
    Engine,
}

/// 任务注册表条目（应用退出即清，v1 不落盘）
#[derive(Debug, Clone, serde::Serialize)]
struct TaskInfo {
    id: String,
    work_dir: String,
    kind: TaskKind,
    status: TaskStatus,
    rounds: i64,
    last_reason: String,
    started_at_ms: u64,
    /// 原始任务描述（重试审查需要）
    task: String,
    /// 工人完成后的会话文件；审查失败后「重试审查」依赖它
    #[serde(skip_serializing_if = "Option::is_none")]
    session_file: Option<String>,
    max_rounds: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    mock: bool,
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
    /// 终端驱动引擎：同目录互斥（一个 Claude pane 同时只能注入一个任务）
    engine_busy_dirs: Mutex<HashSet<String>>,
    /// task_id → 终端驱动引擎（阶段 2）的取消标志
    engine_cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// 任务注册表（含历史终态；应用退出即清，重启后历史消失属预期）
    tasks: Mutex<HashMap<String, TaskInfo>>,
}

fn engine_dir_available(busy: &HashSet<String>, dir: &str) -> bool {
    !busy.contains(dir)
}

fn release_engine_dir(state: &SuperviseState, dir: &str) {
    if let Ok(mut busy) = state.engine_busy_dirs.lock() {
        busy.remove(dir);
    }
}

impl SuperviseState {
    /// 窗口关闭时终止全部监督任务——否则 pwsh + claude/codex 孤儿进程
    /// 会在应用退出后继续无人监督地跑完整轮任务（烧 token）
    fn stop_all(&self) {
        // 先置引擎取消标志：Unix 进程组终止可能数秒，引擎线程应尽快停注入
        if let Ok(flags) = self.engine_cancels.lock() {
            for flag in flags.values() {
                flag.store(true, Ordering::Relaxed);
            }
        }
        // 短锁 drain：终止含宽限等待，持锁会卡住 run_supervise 的 insert
        let children: Vec<std::process::Child> = {
            let Ok(mut running) = self.running.lock() else { return };
            running.drain().map(|(_, c)| c).collect()
        };
        for mut child in children {
            terminate_supervise_child(&mut child);
        }
    }
}

/// 终止一个监督子进程及其全部子孙。
/// Unix：spawn 时已设独立进程组 → 组 SIGTERM → 宽限 → 组 SIGKILL 扫尾；
/// Windows：先 kill_process_tree（父进程仍活着时 taskkill /T）再杀父进程。
fn terminate_supervise_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        supervise_runner::terminate_supervise(child);
    }
    #[cfg(windows)]
    {
        let pid = child.id();
        if let Err(error) = kill_process_tree(Some(pid)) {
            eprintln!("[supervise] 终止监督进程树（PID {pid}）失败: {error}");
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child.kill();
        let _ = child.wait();
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
    agent: String,
    work_dir: String,
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
#[cfg_attr(not(windows), allow(unused_variables))]
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
        agent: request.agent.clone(),
        work_dir: request.work_dir.clone(),
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
    // 无头模式：同目录可并发（产物按 task_id 隔离）。先分配 id 再 spawn，
    // 以便 SUPERVISE_TASK_ID 进入子进程环境。
    let task_id = format!("task-{}", TASK_COUNTER.fetch_add(1, Ordering::Relaxed));

    let mut child = match supervise_runner::spawn_supervise(&request, Some(&task_id)) {
        Ok(c) => c,
        Err(e) => return Err(e),
    };
    let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
    // stderr 必须有人消费：写满管道缓冲会挂死子进程（任务永不收尾）。
    // 逐行并入 supervise-log（带 [stderr] 前缀），诊断信息直接进 UI 日志流。
    let stderr = child.stderr.take();
    state.running.lock().unwrap().insert(task_id.clone(), child);
    // 登记任务注册表（Running 态）。展示用原始路径；busy 锁仍用上面的 normalize 键。
    state.tasks.lock().unwrap().insert(task_id.clone(), TaskInfo {
        id: task_id.clone(),
        work_dir: request.work_dir.clone(),
        kind: TaskKind::Ps1,
        status: TaskStatus::Running,
        rounds: 0,
        last_reason: String::new(),
        started_at_ms: now_ms(),
        task: request.task.clone(),
        session_file: None,
        max_rounds: level_rounds(request.level.as_deref(), request.max_rounds),
        model: request.model.clone(),
        mock: request.mock,
    });

    // 后台线程读 stdout → 逐行 emit 到前端；进程结束后清理 State 并 emit done
    let app2 = app.clone();
    let task_id2 = task_id.clone();
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
        // 更新任务注册表终态
        {
            let mut tasks = running.tasks.lock().unwrap();
            if let Some(t) = tasks.get_mut(&task_id2) {
                t.status = ps1_exit_to_status(exit_code);
            }
        }
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

/// 取消运行中的监督任务（ps1 无头模式杀进程树；终端驱动引擎置取消标志）
#[tauri::command]
async fn cancel_supervise(app: AppHandle, task_id: String) -> Result<(), String> {
    let state = app.state::<SuperviseState>();
    // 短锁：只摘除 entry。终止流程含最多数秒的宽限等待，持锁做会卡死
    // run_supervise 的 insert 与 reader 线程的收尾清理
    let mut child = state
        .running
        .lock()
        .map_err(|_| "监督状态锁已损坏")?
        .remove(&task_id);
    if let Some(c) = child.as_mut() {
        terminate_supervise_child(c);
        return Ok(());
    }
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

    // 定位正在跑的 Claude pane（必须在任务目录上——引擎逐轮校验绑定）。
    // 单次遍历取 (id, dir)：两次 find 在同目录多 pane 时可能各命中不同的
    // pane（HashMap 无序），id 与目录错配
    let session_id = {
        let term = app.state::<TerminalState>();
        let sessions = term.sessions.lock().map_err(|_| "终端状态锁已损坏")?;
        sessions
            .iter()
            .find(|(_, p)| p.agent == "claude" && normalize_path(&p.work_dir) == work_dir)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| {
                "未找到运行中的 Claude 终端（请先在终端工作台以该工作目录启动 Claude CLI）"
                    .to_string()
            })?
    };

    // 目录占用登记（仅引擎：同 Claude pane 不能并行注入）
    {
        let mut busy = state.engine_busy_dirs.lock().unwrap();
        if !engine_dir_available(&busy, &work_dir) {
            return Err("该工作目录已有终端驱动任务在运行（同一 Claude 会话不能并行注入）".into());
        }
        busy.insert(work_dir.clone());
    }

    // Stop hook 幂等安装 + marker 文件（.supervise/stop-markers.jsonl，随项目走）
    let supervise_dir = std::path::Path::new(&request.work_dir).join(".supervise");
    if let Err(e) = std::fs::create_dir_all(&supervise_dir) {
        release_engine_dir(&state, &work_dir);
        return Err(format!("创建 .supervise 失败: {e}"));
    }
    let marker_file = supervise_dir.join("stop-markers.jsonl");
    let home = match std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        Ok(h) => h,
        Err(_) => {
            release_engine_dir(&state, &work_dir);
            return Err("无法定位用户主目录（USERPROFILE/HOME 均缺失）".into());
        }
    };
    let settings = std::path::Path::new(&home).join(".claude").join("settings.json");
    let hook_installed = !request.mock;
    if hook_installed {
        if let Err(e) = ensure_stop_hook(&settings, &marker_file) {
            release_engine_dir(&state, &work_dir);
            return Err(format!("安装 Stop hook 失败（可重试或改用无头模式）: {e}"));
        }
    }

    let rounds = level_rounds(request.level.as_deref(), request.max_rounds);
    let model = request.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
    let task_id = format!("task-{}", TASK_COUNTER.fetch_add(1, Ordering::Relaxed));
    let cancel = Arc::new(AtomicBool::new(false));
    state
        .engine_cancels
        .lock()
        .unwrap()
        .insert(task_id.clone(), cancel.clone());
    // 登记任务注册表（Running 态）。work_dir 必须保留用户/终端原始大小写：
    // normalize 键只用于 busy 锁；引擎用此路径算 project_slug，小写会错位 Claude 会话目录。
    state.tasks.lock().unwrap().insert(task_id.clone(), TaskInfo {
        id: task_id.clone(),
        work_dir: request.work_dir.clone(),
        kind: TaskKind::Engine,
        status: TaskStatus::Running,
        rounds: 0,
        last_reason: String::new(),
        started_at_ms: now_ms(),
        task: request.task.clone(),
        session_file: None,
        max_rounds: rounds,
        model: model.map(|m| m.to_string()),
        mock: request.mock,
    });

    let opts = EngineOptions {
        task: request.task.clone(),
        work_dir: request.work_dir.clone(),
        max_rounds: rounds,
        // 产物落 .supervise（审查看板与「查看会话」跳转消费同一套格式）
        artifacts_dir: Some(supervise_dir.clone()),
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

    let app2 = app.clone();
    let task_id2 = task_id.clone();
    let work_dir2 = work_dir.clone();
    let settings2 = settings.clone();
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
                let _ = app3.emit(
                    "supervise-log",
                    serde_json::json!({ "taskId": task_id3, "line": line.to_string() }),
                );
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
                    session_file: None,
                }
            });

        // 收尾：清登记 →（无其他引擎时）卸载 hook → emit done。
        // Stop hook 指向本项目 marker，遗留会让用户所有 Claude 会话每次
        // Stop 都白跑一次 powershell，多项目还会累积死条目
        let app_state = app2.state::<SuperviseState>();
        let remaining_engines = {
            let mut flags = app_state.engine_cancels.lock().unwrap();
            flags.remove(&task_id2);
            flags.len()
        };
        app_state
            .engine_busy_dirs
            .lock()
            .unwrap()
            .remove(&work_dir2);
        if hook_installed && remaining_engines == 0 {
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
                if let Some(sf) = &outcome.session_file {
                    t.session_file = Some(sf.display().to_string());
                }
            }
        }
        let _ = app2.emit(
            "supervise-done",
            serde_json::json!({ "taskId": task_id2, "exitCode": code, "reason": outcome.last_reason }),
        );
    });

    Ok(task_id)
}

/// 重试审查：复用已中止任务的会话文件，跳过工人，只再跑 Codex。
/// 需同目录 Claude 终端仍在跑（若审查结果为返工，还要继续注入）。
#[tauri::command]
async fn retry_supervise_review(app: AppHandle, task_id: String) -> Result<String, String> {
    let state = app.state::<SuperviseState>();
    let snapshot = {
        let tasks = state.tasks.lock().map_err(|_| "任务状态锁已损坏")?;
        tasks.get(&task_id).cloned().ok_or_else(|| "任务不存在".to_string())?
    };
    if snapshot.kind != TaskKind::Engine {
        return Err("仅终端驱动任务支持重试审查".into());
    }
    if snapshot.status != TaskStatus::Aborted {
        return Err(format!(
            "只有已中止的任务可重试审查（当前状态：{:?}）",
            snapshot.status
        ));
    }
    let session_file = snapshot
        .session_file
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "没有可复用的会话文件（工人未完成或产物已丢失）".to_string())?;
    if !std::path::Path::new(&session_file).is_file() {
        return Err(format!("会话文件不存在：{session_file}"));
    }
    let lock_key = normalize_path(&snapshot.work_dir);
    let resume_round = snapshot.rounds.max(1);

    // 同目录 Claude pane：取其原始大小写路径喂给引擎（旧任务可能把小写 normalize
    // 路径写进 TaskInfo，直接复用会算出错误 slug，Stop hook 对不上）。
    let (session_id, pane_work_dir) = {
        let term = app.state::<TerminalState>();
        let sessions = term.sessions.lock().map_err(|_| "终端状态锁已损坏")?;
        sessions
            .iter()
            .find(|(_, p)| p.agent == "claude" && normalize_path(&p.work_dir) == lock_key)
            .map(|(id, p)| (id.clone(), p.work_dir.clone()))
            .ok_or_else(|| {
                "未找到运行中的 Claude 终端（请先在该工作目录启动 Claude，再点重试审查）"
                    .to_string()
            })?
    };

    {
        let mut busy = state.engine_busy_dirs.lock().unwrap();
        if !engine_dir_available(&busy, &lock_key) {
            return Err(format!(
                "工作目录 {lock_key} 已有终端驱动任务在运行（同一 Claude 会话不能并行注入）"
            ));
        }
        busy.insert(lock_key.clone());
    }

    let supervise_dir = std::path::Path::new(&pane_work_dir).join(".supervise");
    if let Err(e) = std::fs::create_dir_all(&supervise_dir) {
        release_engine_dir(&state, &lock_key);
        return Err(format!("创建 .supervise 失败: {e}"));
    }
    let marker_file = supervise_dir.join("stop-markers.jsonl");
    let home = match std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        Ok(h) => h,
        Err(_) => {
            release_engine_dir(&state, &lock_key);
            return Err("无法定位用户主目录（USERPROFILE/HOME 均缺失）".into());
        }
    };
    let settings = std::path::Path::new(&home).join(".claude").join("settings.json");
    let hook_installed = !snapshot.mock;
    if hook_installed {
        if let Err(e) = ensure_stop_hook(&settings, &marker_file) {
            release_engine_dir(&state, &lock_key);
            return Err(format!("安装 Stop hook 失败: {e}"));
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    state
        .engine_cancels
        .lock()
        .unwrap()
        .insert(task_id.clone(), cancel.clone());
    {
        let mut tasks = state.tasks.lock().unwrap();
        if let Some(t) = tasks.get_mut(&task_id) {
            t.status = TaskStatus::Running;
            t.last_reason = String::new();
            // 纠正可能被 normalize 污染的大小写
            t.work_dir = pane_work_dir.clone();
        }
    }

    let model = snapshot.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
    let opts = EngineOptions {
        task: snapshot.task.clone(),
        work_dir: pane_work_dir,
        max_rounds: snapshot.max_rounds.max(resume_round),
        artifacts_dir: Some(supervise_dir.clone()),
        reviewer_label: match (snapshot.mock, model) {
            (true, _) => "mock".to_string(),
            (false, Some(m)) => m.to_string(),
            (false, None) => "codex 默认模型".to_string(),
        },
        resume_transcript: Some(std::path::PathBuf::from(&session_file)),
        resume_round: Some(resume_round),
        ..Default::default()
    };
    let reviewer: Arc<dyn Reviewer> = if snapshot.mock {
        Arc::new(MockReviewer::always(Ok(Verdict {
            pass: true,
            reason: "（模拟）重试审查通过".into(),
        })))
    } else {
        Arc::new(CodexReviewer::new(model, &snapshot.task))
    };

    let app2 = app.clone();
    let task_id2 = task_id.clone();
    let work_dir2 = lock_key.clone();
    let settings2 = settings.clone();
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
        let on_log: supervise_engine::OnLog = Arc::new(move |line: &str| {
            let _ = app3.emit(
                "supervise-log",
                serde_json::json!({ "taskId": task_id3, "line": line.to_string() }),
            );
        });
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                session_file: Some(std::path::PathBuf::from(&session_file)),
            }
        });

        let app_state = app2.state::<SuperviseState>();
        let remaining_engines = {
            let mut flags = app_state.engine_cancels.lock().unwrap();
            flags.remove(&task_id2);
            flags.len()
        };
        app_state
            .engine_busy_dirs
            .lock()
            .unwrap()
            .remove(&work_dir2);
        if hook_installed && remaining_engines == 0 {
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
                if let Some(sf) = &outcome.session_file {
                    t.session_file = Some(sf.display().to_string());
                }
            }
        }
        let _ = app2.emit(
            "supervise-done",
            serde_json::json!({ "taskId": task_id2, "exitCode": code, "reason": outcome.last_reason }),
        );
    });

    Ok(task_id)
}

/// 读取监督闭环产物（.supervise 目录；无头任务可指定 task_id 读子目录）
#[tauri::command]
async fn read_review_artifacts(
    work_dir: String,
    task_id: Option<String>,
) -> Result<Vec<ReviewArtifact>, String> {
    supervise_runner::read_artifacts(&work_dir, task_id.as_deref())
}

/// 列出全部监督任务（含历史终态；应用退出即清，重启后历史消失属预期）
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

/// GUI 直启（Finder/Dock 启动 .app）继承的是 launchd 的最小 PATH（/usr/bin:/bin:…），
/// 找不到 /opt/homebrew/bin 的 pwsh/node 与用户级 npm bin 的 claude/codex。
/// 采集一次登录交互 shell 的 PATH（-i 补读 ~/.zshrc——nvm 等常把导出写在 zshrc），
/// 把缺失条目追加进当前进程 PATH；此后所有子进程（PTY 终端 / session_proxy /
/// mcp_checker / supervise）经环境继承全部生效。失败静默保持现状（dev 模式本就
/// 继承开发 shell 的完整 PATH，合并幂等无害）。
#[cfg(unix)]
fn augment_path_from_login_shell() {
    use std::process::{Command, Stdio};
    let Ok(out) = Command::new("/bin/zsh")
        .arg("-ilc")
        .arg("printf %s \"$PATH\"")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let login_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if login_path.is_empty() {
        return;
    }
    let current = std::env::var_os("PATH").unwrap_or_default();
    let have: Vec<_> = std::env::split_paths(&current).collect();
    let mut merged = have.clone();
    for dir in std::env::split_paths(&login_path) {
        if !merged.contains(&dir) {
            merged.push(dir);
        }
    }
    if merged != have {
        if let Ok(joined) = std::env::join_paths(merged) {
            std::env::set_var("PATH", joined);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(SuperviseState {
            running: Mutex::new(HashMap::new()),
            engine_busy_dirs: Mutex::new(HashSet::new()),
            engine_cancels: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
        })
        .manage(TerminalState {
            sessions: Mutex::new(HashMap::new()),
        })
        .setup(|app| {
            // GUI 直启时 PATH 不含 homebrew/npm 全局 bin，先补齐（见函数注释）
            #[cfg(unix)]
            augment_path_from_login_shell();
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
            run_supervise_terminal,
            retry_supervise_review,
            read_review_artifacts,
            list_supervise_tasks,
            check_mcp,
            fix_mcp,
            export_transcript_md,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                window.state::<TerminalState>().stop_all();
                // 监督任务同样清理：不清理会留 pwsh/claude/codex 孤儿继续烧 API 额度
                window.state::<SuperviseState>().stop_all();
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
    fn task_info_serializes_to_snake_case() {
        let info = TaskInfo {
            id: "task-1".into(),
            work_dir: "D:\\work".into(),
            kind: TaskKind::Engine,
            status: TaskStatus::Running,
            rounds: 0,
            last_reason: String::new(),
            started_at_ms: now_ms(),
            task: "demo".into(),
            session_file: None,
            max_rounds: 3,
            model: None,
            mock: false,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["kind"], "engine");
        assert_eq!(json["status"], "running");
    }

    #[test]
    fn engine_dir_available_same_dir_busy_other_dir_free() {
        let mut busy = HashSet::new();
        assert!(engine_dir_available(&busy, "D:\\a"));
        busy.insert("D:\\a".into());
        assert!(!engine_dir_available(&busy, "D:\\a"));
        assert!(engine_dir_available(&busy, "D:\\b"));
    }
}

#[cfg(all(test, unix))]
mod path_tests {
    /// 剥到 launchd 级最小 PATH 后调用采集：登录 shell 的条目（含用户级 npm bin、
    /// homebrew）必须被追加进来，且原有条目不丢。改的是进程级环境，测试结束还原。
    /// 注：不能依赖 ps eww 观测——它显示的是 exec 时的环境块快照，看不到运行期
    /// set_var 的修改，必须进程内断言。
    #[test]
    fn augment_path_from_login_shell_merges_missing_entries() {
        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        super::augment_path_from_login_shell();
        let now = std::env::var("PATH").expect("PATH 仍在");
        let count = now.split(':').filter(|s| !s.is_empty()).count();
        assert!(
            count > 4,
            "登录 shell 的 PATH 条目应被追加合并（实际 {count} 条）: {now}"
        );
        for base in ["/usr/bin", "/bin"] {
            assert!(now.contains(base), "原有条目不丢: {now}");
        }
        if let Some(p) = saved {
            std::env::set_var("PATH", p);
        }
    }
}
