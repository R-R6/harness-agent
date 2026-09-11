//! supervise_engine —— 驱动可见 Claude 终端 pane 的监督引擎（阶段 2 MVP）
//!
//! 与 supervise.ps1 的无头模式不同：本引擎不自己 spawn claude，而是把任务/
//! 回灌意见注入用户正在看的终端 pane（PaneIo 回调），干活过程全程可见、
//! 人可随时插手。轮次完成信号以 Stop hook 写入的 marker 为主（spike B：
//! stdin 自带 session_id + transcript_path），JSONL 静默阈值 + 单轮硬超时
//! 双兜底（spike A：交互会话实时增量落盘）。
//!
//! 本 crate 不依赖 tauri（cargo test 可跑）；PTY 注入由 lib.rs 的适配器实现。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

pub mod hook;
pub mod review;

pub use review::{CodexReviewer, MockReviewer, Reviewer, Verdict};

/// 注册表查询转发（审查命令构造需要 profile 声明）
pub fn agent_profile(id: &str) -> Option<agent_registry::AgentProfile> {
    agent_registry::get(id).copied()
}

// ---------------- 引擎契约 ----------------

/// 终端 pane 的 IO 通道（lib.rs 用 TerminalState 的 PTY writer 实现）
pub trait PaneIo: Send + Sync {
    /// 注入文本到 pane（引擎负责追加回车）
    fn write(&self, data: &str) -> Result<(), String>;
    /// pane 当前绑定的工作目录（会话被停掉/替换/目录不符时返回 None 或不等的值，
    /// 引擎据此中止——防止意见注入到错误目录的会话）
    fn current_work_dir(&self) -> Option<String>;
    /// 最近一段 PTY 输出（用于识别信任对话框 / 输入栏是否已出现）。测试假 pane 默认为空。
    fn recent_output(&self) -> String {
        String::new()
    }
}

/// 日志回调（lib.rs 转 supervise-log 事件）
pub type OnLog = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub task: String,
    /// pane 必须绑定的目录（绑定校验用，注入前逐轮检查）
    pub work_dir: String,
    pub max_rounds: i64,
    /// 起始轮次偏移（默认 0）：「再来一轮」续跑时从原轮数继续（原 3 轮 → 第 4/4 轮）
    pub starting_round: i64,
    /// JSONL 静默兜底阈值（默认 180s。实测 Claude 跑长工具/测试时 2 分钟
    /// 不写会话文件是常态，120s 会把还在干活的轮次误判成"已完成"）
    pub silence: Duration,
    /// 单轮硬超时（默认 15min）
    pub round_timeout: Duration,
    pub poll_interval: Duration,
    /// 注入送达确认窗口（默认 15s）：注入后在会话文件里等它变成新的用户行，
    /// 超时则重发一次（真实事故：空闲/away 的 TUI 吞掉注入文本，工人整轮空转）
    pub delivery_confirm: Duration,
    /// 首轮额外确认窗口（默认 30s）：Claude Code 冷启动（加载原生程序+连 API）
    /// 在输入就绪前可达十几秒，单发 \r 会被吞、正文停在输入栏；首轮确认用
    /// delivery_confirm + 本字段，期间每 2s 补回车，覆盖启动期。
    pub first_inject_confirm: Duration,
    /// 首轮注入前等待窗口（默认 60s）：信任菜单可见时只等用户手动处理；
    /// 菜单消失或从未出现后注入。测试设 0 跳过。
    pub input_ready_wait: Duration,
    /// 产物目录（.supervise/tasks/<id> 或测试目录）：逐轮 review-N.md + final-report.json。
    /// None 则不落盘（纯测试用）。只清 review-*.md/final-report.json，不整目录删。
    pub artifacts_dir: Option<PathBuf>,
    /// 任务令牌：注入文案带 `[supervise-task:<token>]`，首轮据此在会话文件中预钉
    /// Claude session，避免同目录多引擎共享 stop-markers 时首轮互抢。
    pub task_token: Option<String>,
    /// 审查者标签（写产物用：模型名 / "mock"）
    pub reviewer_label: String,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            task: String::new(),
            work_dir: String::new(),
            max_rounds: 3,
            starting_round: 0,
            silence: Duration::from_secs(180),
            round_timeout: Duration::from_secs(15 * 60),
            poll_interval: Duration::from_millis(500),
            delivery_confirm: Duration::from_secs(15),
            first_inject_confirm: Duration::from_secs(30),
            input_ready_wait: Duration::from_secs(60),
            artifacts_dir: None,
            task_token: None,
            reviewer_label: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineStatus {
    /// 全部轮次内验收通过
    Accepted,
    /// 轮次用完仍未通过
    Rejected,
    /// 用户取消
    Cancelled,
    /// pane 绑定失效（会话退出/目录被换）——中止防误注入
    Aborted(String),
}

#[derive(Debug, Clone)]
pub struct EngineOutcome {
    pub status: EngineStatus,
    pub rounds: i64,
    pub last_reason: String,
}

// ---------------- Stop hook marker 消费 ----------------

/// Stop hook 把 stdin JSON 逐行追加到 marker 文件；引擎只消费启动之后新增的行
/// （快照初始行数，规避 timestamp 解析；其他项目/会话的 marker 天然被过滤）
pub struct MarkerSource {
    path: PathBuf,
    /// 已消费到的行数（游标）：read_new 只返回其后的新行并推进，
    /// 防止上一轮的旧 marker 在下一轮被重复消费
    consumed: std::cell::Cell<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct StopMarker {
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
}

impl MarkerSource {
    pub fn new(path: PathBuf) -> Self {
        let initial_lines = read_lines(&path).len();
        Self {
            path,
            consumed: std::cell::Cell::new(initial_lines),
        }
    }

    /// 自上次调用以来新增的 markers（坏行跳过、游标推进）。
    /// 必须推进游标：无游标版每次都返回启动后的全部 marker，第 2 轮等待时
    /// 旧 marker 会立即短路轮次状态机（第 1 轮的 Stop 信号被重复消费）。
    pub fn read_new(&self) -> Vec<StopMarker> {
        let lines = read_lines(&self.path);
        let mut start = self.consumed.get();
        if lines.len() < start {
            // 文件被截断/重建：全部视为新事件
            start = 0;
        }
        let mut out = vec![];
        for (i, line) in lines.into_iter().enumerate().skip(start) {
            self.consumed.set(i + 1);
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                out.push(StopMarker {
                    session_id: v.get("session_id").and_then(|s| s.as_str()).map(String::from),
                    transcript_path: v
                        .get("transcript_path")
                        .and_then(|s| s.as_str())
                        .map(String::from),
                });
            }
        }
        out
    }
}

/// 数会话文件中的"用户文本行"（type=user 且 message.content 为非空字符串——
/// 注入的任务/返工文本正是这种形态；工具结果行 content 是数组，不计入）。
/// 用于注入送达确认：注入生效必然让计数 +1。
fn count_user_inputs(path: &Path) -> Option<usize> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut n = 0usize;
    for line in text.lines() {
        // 廉价预筛避免整文件逐行 JSON 解析
        if !line.contains("\"type\":\"user\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let is_user_text = v.get("type").and_then(|t| t.as_str()) == Some("user")
            && v.get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .is_some_and(|s| !s.trim().is_empty());
        if is_user_text {
            n += 1;
        }
    }
    Some(n)
}

/// 等 path 的用户行计数超过 before（截止 deadline 前轮询）
/// 注入提交器：等待 confirm() 为 true，期间每 2s 补一个回车，把暂留在 Claude 输入栏
/// 的正文提交。Claude TUI 未就绪/渲染中会吞掉单次 \r（真实事故：正文停在输入栏等
/// 手动回车；重写正文又造成「任务两遍」）——窗口内只补回车、不重写正文，故不会两遍。
fn confirm_inject<F: FnMut() -> bool>(
    mut confirm: F,
    pane: &Arc<dyn PaneIo>,
    deadline: Instant,
    poll: Duration,
    cancel: Option<&AtomicBool>,
) -> bool {
    let mut last_enter = Instant::now();
    while Instant::now() < deadline {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return false;
        }
        if confirm() {
            return true;
        }
        if last_enter.elapsed() >= Duration::from_millis(2000) {
            // 只补回车。信任菜单只在 wait_for_input_ready 里等待用户处理，这里不改键。
            let _ = pane.write("\r");
            last_enter = Instant::now();
        }
        std::thread::sleep(poll);
    }
    !cancel.is_some_and(|c| c.load(Ordering::Relaxed)) && confirm()
}

fn visible_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for x in chars.by_ref() {
                        if ('@'..='~').contains(&x) {
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// 只看输出尾部：PTY recent_output 会保留旧帧，「Yes, I trust」在信任过后仍可能留在缓冲里。
fn output_tail(output: &str) -> String {
    const TAIL: usize = 4000;
    let vis = visible_text(output);
    if vis.len() <= TAIL {
        vis
    } else {
        // 中文 TUI 文案是多字节 UTF-8，起始下标必须落在字符边界上
        let mut start = vis.len() - TAIL;
        while !vis.is_char_boundary(start) {
            start += 1;
        }
        vis[start..].to_string()
    }
}

/// TUI 输入栏的可见标记（就绪/负向判据共用）。不作为注入的必要条件——
/// 真机 TUI 文案/ANSI 变体对不上时不得因此空跑整轮。
const INPUT_BAR_MARKERS: [&str; 3] = ["for shortcuts", "bypassing permissions", "shift+tab"];

fn has_input_bar_marker(s: &str) -> bool {
    INPUT_BAR_MARKERS.iter().any(|m| s.contains(m))
}

fn looks_like_trust_prompt(output: &str) -> bool {
    let s = output_tail(output).to_ascii_lowercase();
    // 输入栏已出现则当前不是信任菜单（尾部可能同时有历史碎片）
    if has_input_bar_marker(&s) {
        return false;
    }
    s.contains("yes, i trust")
        || s.contains("trust this folder")
        || s.contains("do you trust")
        || s.contains("i trust this folder")
        || s.contains("no, exit")
        || s.contains("信任此文件夹")
        || s.contains("是否信任")
}

fn looks_like_input_ready(output: &str) -> bool {
    if looks_like_trust_prompt(output) {
        return false;
    }
    let s = output_tail(output).to_ascii_lowercase();
    has_input_bar_marker(&s) || s.contains("welcome to claude")
}

/// 首轮注入前：若屏幕上仍是文件夹信任菜单，只等待用户手动选完，绝不灌任务正文、
/// 也不自动按键。菜单消失（或从未出现）后即可注入——不要求必须匹配
/// `? for shortcuts` 等就绪文案（真机：TUI 文案/ANSI 变体对不上时会空跑整轮）。
fn wait_for_input_ready(
    opts: &EngineOptions,
    pane: &Arc<dyn PaneIo>,
    cancel: &AtomicBool,
    on_log: &OnLog,
) -> Result<(), String> {
    if opts.input_ready_wait.is_zero() {
        return Ok(());
    }
    on_log("[ENGINE] 等待 Claude 终端就绪（若出现信任对话框请手动选 Yes，选完后自动注入）…");
    let started = Instant::now();
    let deadline = started + opts.input_ready_wait;
    let mut logged_trust = false;
    let mut clear_since: Option<Instant> = None;
    // 从未见到信任菜单时，给 TUI 一点冷启动时间再注入，避免刚 spawn 就灌键。
    let startup_grace = Duration::from_secs(2).min(opts.input_ready_wait);
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消，未注入任务".into());
        }
        let out = pane.recent_output();
        if looks_like_trust_prompt(&out) {
            clear_since = None;
            if !logged_trust {
                on_log("[ENGINE] 检测到文件夹信任对话框，请手动选择 Yes；选完后继续注入…");
                logged_trust = true;
            }
            std::thread::sleep(opts.poll_interval);
            continue;
        }
        if logged_trust {
            let since = *clear_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_millis(500) {
                on_log("[ENGINE] 信任对话框已消失，开始注入");
                return Ok(());
            }
        } else if looks_like_input_ready(&out) || started.elapsed() >= startup_grace {
            on_log("[ENGINE] Claude 终端已就绪，开始注入");
            return Ok(());
        }
        std::thread::sleep(opts.poll_interval);
    }
    if looks_like_trust_prompt(&pane.recent_output()) {
        return Err(
            "Claude 仍停在文件夹信任对话框，未注入以免卡死选择菜单（请先手动选 Yes 后再启动）"
                .into(),
        );
    }
    // 超时但仍不像信任菜单：宁可注入，也不要空跑整轮（真机事故：等就绪文案导致零注入）。
    on_log("[ENGINE] 等待就绪超时且未见信任对话框，继续注入");
    Ok(())
}

fn read_lines(path: &Path) -> Vec<String> {    match std::fs::read(path) {
        Ok(bytes) => {
            // 容错解码：旧版 hook 或异常代码页可能写出非 UTF-8 字节；lossy
            // 替换后 ASCII 字段（session_id/transcript_path）仍完好可解析
            let text = String::from_utf8_lossy(&bytes);
            let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
            // PS5.1 的 UTF8 首次建文件带 BOM，不剥掉会让首个 marker 解析失败
            if let Some(first) = lines.first_mut() {
                *first = first.trim_start_matches('\u{feff}').to_string();
            }
            lines
        }
        Err(_) => Vec::new(),
    }
}

// ---------------- 会话文件定位（marker 缺失时的兜底） ----------------

/// Claude 项目 slug 是有损编码（实测 F:\project\workspace-side\my_skils →
/// F--project-workspace-side-my-skils）：每个非字母数字且非连字符的字符
/// 各自折叠为一个 '-'（连续分隔符不合并——冒号和反斜杠产出 "--"）。
pub fn project_slug(dir: &str) -> String {
    dir.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// 项目 slug 目录里 mtime >= cutoff 的最新会话文件（引擎启动后活跃的那个）
pub fn newest_session_after(projects_root: &Path, slug: &str, cutoff: SystemTime) -> Option<PathBuf> {
    let entries = std::fs::read_dir(projects_root.join(slug)).ok()?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            continue;
        };
        if mtime < cutoff {
            continue;
        }
        if best.as_ref().is_none_or(|(t, _)| mtime >= *t) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

/// 在 slug 目录中找内容含 token 的会话文件（多进程首轮预钉用）
pub fn find_session_containing(projects_root: &Path, slug: &str, token: &str) -> Option<PathBuf> {
    if token.is_empty() {
        return None;
    }
    let entries = std::fs::read_dir(projects_root.join(slug)).ok()?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        if !String::from_utf8_lossy(&bytes).contains(token) {
            continue;
        }
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let key = mtime.unwrap_or(SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| key >= *t) {
            best = Some((key, path));
        }
    }
    best.map(|(_, p)| p)
}

fn session_id_of_transcript(path: &Path) -> Option<String> {
    path.file_stem().map(|s| s.to_string_lossy().to_string())
}

// ---------------- 轮次等待：marker 主信号 + 静默兜底 + 硬超时 ----------------

struct TranscriptWatch {
    path: PathBuf,
    last_size: u64,
    last_change: Instant,
    saw_activity: bool,
}

impl TranscriptWatch {
    fn new(path: PathBuf) -> Self {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self {
            path,
            last_size: size,
            last_change: Instant::now(),
            // 已有内容即视为活动过（静默兜底要求"见过活动"才生效，
            // 防止刚注入还没开写就误判完成）
            saw_activity: size > 0,
        }
    }

    fn poll(&mut self) {
        match std::fs::metadata(&self.path) {
            Ok(meta) => {
                let size = meta.len();
                if size != self.last_size {
                    self.last_size = size;
                    self.last_change = Instant::now();
                    self.saw_activity = true;
                }
            }
            Err(_) => {
                self.last_size = 0; // 文件被轮转：等它重新出现
            }
        }
    }
}

/// 一轮的结束方式
#[derive(Debug, Clone, PartialEq)]
pub enum RoundEnd {
    /// Stop hook marker（最可信）
    StopMarker,
    /// transcript 静默达标
    Silence,
    /// 单轮硬超时（仍进入审查，让审查者裁决）
    Timeout,
    Cancelled,
}

fn wait_round_end(
    opts: &EngineOptions,
    markers: &MarkerSource,
    projects_root: &Path,
    slug: &str,
    cutoff: SystemTime,
    // 会话锁定：首轮确定后只认该会话的 marker——同目录其他 Claude 会话
    // （外部终端/别的 pane）Stop 时不能串台结束本轮
    session_pin: &mut Option<String>,
    cancel: &AtomicBool,
) -> (Option<PathBuf>, RoundEnd) {
    let start = Instant::now();
    let mut transcript: Option<PathBuf> = None;
    let mut watch: Option<TranscriptWatch> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return (transcript, RoundEnd::Cancelled);
        }
        // 主信号：本项目 slug 目录内、且（已锁定会话时）属于该会话的新 marker
        for m in markers.read_new() {
            let Some(tp) = &m.transcript_path else { continue };
            let in_project = Path::new(tp)
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n.to_string_lossy() == slug);
            let session_ok = session_pin
                .as_ref()
                .map(|p| m.session_id.as_deref() == Some(p.as_str()))
                .unwrap_or(true);
            if in_project && session_ok {
                let path = PathBuf::from(tp);
                if let Some(sid) = &m.session_id {
                    *session_pin = Some(sid.clone());
                } else if let Some(stem) = path.file_stem() {
                    *session_pin = Some(stem.to_string_lossy().to_string());
                }
                return (Some(path), RoundEnd::StopMarker);
            }
        }
        // 兜底定位：已预钉则只跟该会话文件；否则取启动后最新（单进程兼容）
        if transcript.is_none() {
            transcript = session_pin
                .as_ref()
                .and_then(|sid| {
                    let p = projects_root.join(slug).join(format!("{sid}.jsonl"));
                    p.exists().then_some(p)
                })
                .or_else(|| newest_session_after(projects_root, slug, cutoff));
        }
        // 静默兜底（只在拿到 transcript 后启用）
        if let Some(tp) = transcript.clone() {
            let w = watch.get_or_insert_with(|| TranscriptWatch::new(tp));
            w.poll();
            if w.saw_activity && w.last_change.elapsed() >= opts.silence {
                return (transcript, RoundEnd::Silence);
            }
        }
        if start.elapsed() >= opts.round_timeout {
            return (transcript, RoundEnd::Timeout);
        }
        std::thread::sleep(opts.poll_interval);
    }
}

// ---------------- 引擎主循环 ----------------

/// 单轮记录（产物落盘用；与 supervise_runner::VerdictEntry 契约对齐）
struct RoundRecord {
    round: i64,
    pass: bool,
    reason: String,
    transcript: Option<PathBuf>,
}

/// 跑完整监督循环。阻塞直到结束/取消/中止（调用方放后台线程）。
/// 若 opts.artifacts_dir 有值，逐轮写 review-N.md、结束写 final-report.json
/// （审查看板与「查看会话」跳转消费同一套格式）。
#[allow(clippy::too_many_arguments)]
pub fn run(
    opts: &EngineOptions,
    pane: Arc<dyn PaneIo>,
    reviewer: Arc<dyn Reviewer>,
    markers: &MarkerSource,
    projects_root: &Path,
    cancel: &AtomicBool,
    on_log: &OnLog,
) -> EngineOutcome {
    let mut records: Vec<RoundRecord> = Vec::new();
    let outcome = run_loop(opts, pane, reviewer, markers, projects_root, cancel, on_log, &mut records);

    if let Some(dir) = &opts.artifacts_dir {
        let stem = |p: &Option<PathBuf>| {
            p.as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        let path_str = |p: &Option<PathBuf>| {
            p.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        };
        let status = match &outcome.status {
            EngineStatus::Accepted => "accepted",
            EngineStatus::Rejected => "rejected",
            EngineStatus::Cancelled => "cancelled",
            EngineStatus::Aborted(_) => "aborted",
        };
        let last = records.last().and_then(|r| r.transcript.clone());
        let verdicts: Vec<serde_json::Value> = records
            .iter()
            .map(|r| {
                serde_json::json!({
                    "round": r.round,
                    "verdict": if r.pass { "PASS" } else { "REVIEW" },
                    "reason": r.reason,
                    "sessionId": stem(&r.transcript),
                    "file": path_str(&r.transcript),
                })
            })
            .collect();
        let report = serde_json::json!({
            "status": status,
            "task": opts.task,
            "rounds": outcome.rounds,
            // 结束原因（accepted=通过理由 / rejected=最后返工意见 / aborted=失败原因）。
            // 真实事故：aborted 时报告只有状态没有原因，用户无法得知为何失败
            "reason": outcome.last_reason,
            "sessionId": stem(&last),
            "sessionFile": path_str(&last),
            "verdicts": verdicts,
        });
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(
            dir.join("final-report.json"),
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        );
    }

    outcome
}

/// 只清本引擎自己的产物，不整目录删——stop-markers.jsonl 也在 .supervise 里，
/// MarkerSource 正快照着它，删了会让轮次主信号失效
fn reset_artifacts(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if (name.starts_with("review-") && name.ends_with(".md")) || name == "final-report.json" {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn write_round_artifact(dir: &Path, label: &str, rec: &RoundRecord) {
    let session_id = rec
        .transcript
        .as_ref()
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let file = rec
        .transcript
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let verdict = if rec.pass { "PASS" } else { "REVIEW" };
    let md = format!(
        "# 第 {} 轮审查意见\n\n- 时间：{}\n- 审查模型：{}\n- 判定：{}\n- 会话：{}\n- 会话文件：{}\n\n## 意见\n\n{}\n",
        rec.round,
        unix_ts_string(),
        label,
        verdict,
        session_id,
        file,
        rec.reason
    );
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(format!("review-{}.md", rec.round)), md);
}

/// unix 秒 → UTC "YYYY-MM-DD HH:MM:SSZ"（不引 chrono 的最小实现，civil-from-days）
fn unix_ts_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (h, m, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    let days = (secs / 86400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mth <= 2);
    format!("{y:04}-{mth:02}-{d:02} {h:02}:{m:02}:{s:02}Z")
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    opts: &EngineOptions,
    pane: Arc<dyn PaneIo>,
    reviewer: Arc<dyn Reviewer>,
    markers: &MarkerSource,
    projects_root: &Path,
    cancel: &AtomicBool,
    on_log: &OnLog,
    records: &mut Vec<RoundRecord>,
) -> EngineOutcome {
    let slug = project_slug(&opts.work_dir);
    let started_at = SystemTime::now();
    // 兜底定位的 mtime 截止：留 5s 容差——会话文件可能恰在引擎启动前后几毫秒
    // 内创建/落盘，严格 >= 会因毫秒级竞态漏掉
    let cutoff = started_at
        .checked_sub(Duration::from_secs(5))
        .unwrap_or(started_at);
    on_log(&format!(
        "[ENGINE] 监督引擎启动：{} 轮，pane 目录 {}（slug {}）",
        opts.max_rounds, opts.work_dir, slug
    ));
    if let Some(dir) = &opts.artifacts_dir {
        reset_artifacts(dir);
    }

    if opts.starting_round == 0 && !opts.input_ready_wait.is_zero() {
        match wait_for_input_ready(opts, &pane, cancel, on_log) {
            Ok(()) => {}
            Err(msg) => {
                // 取消与中止统一走 Err 返回，由 cancel 标志区分语义（不做字符串协议）
                if cancel.load(Ordering::Relaxed) {
                    on_log("[ENGINE] 已取消");
                    return EngineOutcome {
                        status: EngineStatus::Cancelled,
                        rounds: 0,
                        last_reason: String::new(),
                    };
                }
                on_log(&format!("[ENGINE] {msg}"));
                return EngineOutcome {
                    status: EngineStatus::Aborted(msg.clone()),
                    rounds: 0,
                    last_reason: msg,
                };
            }
        }
    }

    let mut last_reason = String::new();
    let mut first_inject = true;
    let mut session_pin: Option<String> = None;
    // 上一轮的会话文件（供下一轮注入送达确认）；上一轮审查时的文件指纹
    // （大小+mtime，供复读守卫判定"会话无变化"）
    let mut known_transcript: Option<PathBuf> = None;
    let mut last_review_stat: Option<(u64, Option<SystemTime>)> = None;
    let mut round: i64 = opts.starting_round;
    while round < opts.max_rounds {
        if cancel.load(Ordering::Relaxed) {
            on_log("[ENGINE] 已取消");
            return EngineOutcome {
                status: EngineStatus::Cancelled,
                rounds: round,
                last_reason,
            };
        }
        round += 1;

        // 绑定校验：pane 会话还活着且目录未换（防误注入）
        match pane.current_work_dir() {
            Some(dir) if same_dir(&dir, &opts.work_dir) => {}
            other => {
                let msg = format!(
                    "pane 绑定失效（期望 {}，实际 {:?}），中止防误注入",
                    opts.work_dir, other
                );
                on_log(&format!("[ENGINE] {msg}"));
                return EngineOutcome {
                    status: EngineStatus::Aborted(msg),
                    rounds: round - 1,
                    last_reason,
                };
            }
        }

        // 首次注入任务文本；此后每轮注入上一轮的返工意见。审查失败的重试
        // 不重新注入（见下方审查重试循环），first_inject 只认第一次真正注入。
        // 反计划模式指令：真实事故——工人对"补测试"自作主张进入计划模式，
        // 写完计划等确认卡死，静默判停把"等确认"当"干完"，白烧一轮
        // 注入文本去提交键：Ink TUI 把「正文+回车」当整块粘贴，尾部 \r 会被当
        // 粘贴结束吞掉，正文停在输入栏等手动回车（真实反馈：任务/返工意见都要
        // 手动回车才执行，且送达确认误报「注入未确认」）。提交键由 send_inject
        // 单独发送；返工意见合成单行（不再中间 \r 打点）。
        let inject = if first_inject {
            match opts.task_token.as_deref().filter(|s| !s.is_empty()) {
                Some(tok) => format!(
                    "[supervise-task:{tok}] {}（直接执行并直接创建/修改文件，不要进入计划模式，不要等待确认）",
                    opts.task
                ),
                None => format!(
                    "{}（直接执行并直接创建/修改文件，不要进入计划模式，不要等待确认）",
                    opts.task
                ),
            }
        } else {
            format!(
                "上一轮审查未通过，请按要求返工：{}（直接动手修改文件并运行验证，禁止进入计划模式或等待确认；如已在计划模式请立即退出并执行）",
                last_reason
            )
        };
        first_inject = false;
        // 唤醒回车 + 正文（不写提交键）：提交由 confirm_inject 的「每 2s 补回车」完成。
        // 单发 \r 会被 TUI 吞掉、正文停在输入栏等手动回车；若重写正文又造成「两遍」
        //（真实证据：任务文字被提交两次）。唤醒回车仅让 away/未就绪的 TUI 进入可输入态。
        let send_inject = |pane: &Arc<dyn PaneIo>| -> Result<(), String> {
            let _ = pane.write("\r"); // 唤醒：Claude 空闲/未就绪时可能吞掉直接输入的文本
            std::thread::sleep(Duration::from_millis(800));
            pane.write(&inject)
        };
        if let Err(e) = send_inject(&pane) {
            let msg = format!("注入失败：{e}");
            on_log(&format!("[ENGINE] {msg}"));
            return EngineOutcome {
                status: EngineStatus::Aborted(msg),
                rounds: round - 1,
                last_reason,
            };
        }
        // 注入送达确认：已知会话文件时，确认注入真的变成会话里的新用户行。
        // 失败重发一次；仍失败只告警不中止——保守保留旧行为超集（首轮没有
        // 已知会话文件，跳过确认）
        if let (Some(path), Some(before)) = (
            known_transcript.as_deref(),
            known_transcript.as_deref().and_then(count_user_inputs),
        ) {
            let path = path.to_path_buf();
            let mut confirmed = confirm_inject(
                || count_user_inputs(&path).is_some_and(|n| n > before),
                &pane,
                Instant::now() + opts.delivery_confirm,
                opts.poll_interval,
                None,
            );
            if !confirmed {
                on_log("[ENGINE] 注入未在会话中确认，重发一次…");
                if let Err(e) = send_inject(&pane) {
                    on_log(&format!("[ENGINE] 重发注入失败：{e}"));
                }
                confirmed = confirm_inject(
                    || count_user_inputs(&path).is_some_and(|n| n > before),
                    &pane,
                    Instant::now() + opts.delivery_confirm,
                    opts.poll_interval,
                    None,
                );
                if !confirmed {
                    on_log("[ENGINE] 警告：注入疑似被终端空闲状态吞掉，工人本轮可能空转");
                }
            }
        }
        on_log(&format!(
            "[ENGINE] 第 {round}/{} 轮已注入，等待干活完成…",
            opts.max_rounds
        ));

        // 首轮：用注入令牌预钉 Claude session（避免多引擎抢同一 Stop）。
        // 提交由 confirm_inject 补回车完成；窗口 = delivery_confirm + first_inject_confirm
        //（覆盖 Claude 冷启动十几秒）。窗口内未确认 = 正文被吞（输入栏空）→ 重写正文
        // 再等一个窗口。窗口内只补回车不重写，故不会「任务两遍」。
        if session_pin.is_none() {
            if let Some(tok) = opts.task_token.as_deref().filter(|s| !s.is_empty()) {
                let token = format!("[supervise-task:{tok}]");
                let window = opts.delivery_confirm + opts.first_inject_confirm;
                for attempt in 0..2 {
                    let pinned = confirm_inject(
                        || {
                            if let Some(path) = find_session_containing(projects_root, &slug, &token) {
                                if let Some(sid) = session_id_of_transcript(&path) {
                                    on_log(&format!("[ENGINE] 已预钉 Claude 会话 {sid}"));
                                    session_pin = Some(sid);
                                    known_transcript = Some(path);
                                }
                            }
                            session_pin.is_some()
                        },
                        &pane,
                        Instant::now() + window,
                        opts.poll_interval,
                        Some(cancel),
                    );
                    if pinned {
                        break;
                    }
                    if cancel.load(Ordering::Relaxed) {
                        return EngineOutcome {
                            status: EngineStatus::Cancelled,
                            rounds: round - 1,
                            last_reason,
                        };
                    }
                    if attempt == 0 {
                        on_log("[ENGINE] 令牌未落盘，重发首轮注入…");
                        if let Err(e) = send_inject(&pane) {
                            let msg = format!("重发首轮注入失败：{e}");
                            on_log(&format!("[ENGINE] {msg}"));
                            return EngineOutcome {
                                status: EngineStatus::Aborted(msg),
                                rounds: round - 1,
                                last_reason,
                            };
                        }
                    }
                }
                if session_pin.is_none() {
                    let msg = "未能按令牌预钉 Claude 会话，中止以防同目录任务串台".to_string();
                    on_log(&format!("[ENGINE] {msg}"));
                    return EngineOutcome {
                        status: EngineStatus::Aborted(msg.clone()),
                        rounds: round - 1,
                        last_reason: msg,
                    };
                }
            }
        }

        let (transcript, ended) =
            wait_round_end(opts, markers, projects_root, &slug, cutoff, &mut session_pin, cancel);
        if ended == RoundEnd::Cancelled {
            return EngineOutcome {
                status: EngineStatus::Cancelled,
                rounds: round,
                last_reason,
            };
        }
        on_log(&format!(
            "[ENGINE] 第 {round} 轮结束（{}）",
            match ended {
                RoundEnd::StopMarker => "Stop hook 信号",
                RoundEnd::Silence => "会话静默",
                RoundEnd::Timeout => "单轮超时",
                RoundEnd::Cancelled => unreachable!(),
            }
        ));

        let Some(transcript) = transcript else {
            last_reason = "未找到会话文件（无 marker 也无新会话），无法审查".into();
            on_log(&format!("[ENGINE] {last_reason}"));
            continue;
        };
        known_transcript = Some(transcript.clone());

        // 复读守卫（确定性，防假 PASS）：会话文件与上轮审查时完全相同
        // （大小+mtime）说明工人未响应返工（注入可能被吞）——直接维持 REVIEW，
        // 不调审查。真实事故：审查者复读一份未变化的会话却放行 PASS，
        // 而返工意见一条都没落实（re 导入还在、测试文件不存在）
        let stat_now = std::fs::metadata(&transcript)
            .ok()
            .map(|m| (m.len(), m.modified().ok()));
        if last_review_stat.is_some() && stat_now == last_review_stat {
            last_reason =
                "会话自上轮审查后无任何变化：工人未响应返工（注入可能被终端吞掉），维持 REVIEW"
                    .into();
            on_log(&format!(
                "[ENGINE] 第 {round} 轮会话无变化，维持 REVIEW（跳过重复审查）"
            ));
            records.push(RoundRecord {
                round,
                pass: false,
                reason: last_reason.clone(),
                transcript: Some(transcript.clone()),
            });
            if let Some(dir) = &opts.artifacts_dir {
                write_round_artifact(dir, &opts.reviewer_label, records.last().expect("刚 push"));
            }
            continue;
        }

        // 审查：Reviewer 内部自带重试（CodexReviewer 5 次 × 8s），引擎层不再
        // 叠加重试——两层嵌套最坏 15 次 codex exec、单轮可拖约 20 分钟，
        // 且取消延迟被放大。审查硬失败即中止（失败信息落日志与产物）
        if cancel.load(Ordering::Relaxed) {
            return EngineOutcome {
                status: EngineStatus::Cancelled,
                rounds: round,
                last_reason,
            };
        }
        // 审查可能持续数分钟：开工前明示，否则用户面对静止的日志分不清
        // "正在审查"和"卡住了"（真实反馈：截图问"这是在审查吗"）
        on_log(&format!(
            "[ENGINE] 第 {round} 轮审查中（{}），通常需要一到几分钟…",
            opts.reviewer_label
        ));
        let verdict = match reviewer.review(&transcript, round, cancel) {
            Ok(v) => v,
            Err(e) => {
                last_reason = format!("审查失败：{e}");
                on_log(&format!("[FAIL] {last_reason}"));
                return EngineOutcome {
                    status: EngineStatus::Aborted(last_reason.clone()),
                    rounds: round,
                    last_reason,
                };
            }
        };
        // 审查后的文件指纹作为下一轮复读守卫的基线
        last_review_stat = std::fs::metadata(&transcript)
            .ok()
            .map(|m| (m.len(), m.modified().ok()));

        // 产物：逐轮落盘（运行中即可在看板刷新看到），报告在 run() 收尾统一写
        records.push(RoundRecord {
            round,
            pass: verdict.pass,
            reason: verdict.reason.clone(),
            transcript: Some(transcript.clone()),
        });
        if let Some(dir) = &opts.artifacts_dir {
            write_round_artifact(dir, &opts.reviewer_label, records.last().expect("刚 push"));
        }

        if verdict.pass {
            on_log(&format!("[PASS] 第 {round} 轮验收通过：{}", verdict.reason));
            return EngineOutcome {
                status: EngineStatus::Accepted,
                rounds: round,
                last_reason: verdict.reason,
            };
        }
        last_reason = verdict.reason.clone();
        on_log(&format!("[REVIEW] 第 {round} 轮需返工：{}", verdict.reason));
    }

    on_log(&format!(
        "[FAIL] 达到最大轮数 {}，未通过验收。最后意见：{}",
        opts.max_rounds, last_reason
    ));
    EngineOutcome {
        status: EngineStatus::Rejected,
        rounds: opts.max_rounds,
        last_reason,
    }
}

/// 目录等价比较（大小写不敏感 + 分隔符归一）
fn same_dir(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

// ---------------- 测试 ----------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sv-engine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 假 pane：记录注入内容；可注入后异步模拟 Claude 写 transcript + Stop marker
    struct FakePane {
        dir_ok: Mutex<Option<String>>,
        writes: Mutex<Vec<String>>,
        fail_write: AtomicBool,
        recent: Mutex<String>,
    }

    impl FakePane {
        fn ok(dir: impl Into<String>) -> Arc<Self> {
            Arc::new(Self {
                dir_ok: Mutex::new(Some(dir.into())),
                writes: Mutex::new(vec![]),
                fail_write: AtomicBool::new(false),
                recent: Mutex::new(String::new()),
            })
        }
    }

    impl PaneIo for FakePane {
        fn write(&self, data: &str) -> Result<(), String> {
            if self.fail_write.load(Ordering::Relaxed) {
                return Err("PTY 已断开".into());
            }
            self.writes.lock().unwrap().push(data.to_string());
            Ok(())
        }
        fn current_work_dir(&self) -> Option<String> {
            self.dir_ok.lock().unwrap().clone()
        }
        fn recent_output(&self) -> String {
            self.recent.lock().unwrap().clone()
        }
    }

    /// 模拟一轮：pane 注入回调里写 transcript 行 + 追加 marker（异步线程，带延迟）
    fn simulate_worker(
        pane: Arc<FakePane>,
        transcript: PathBuf,
        marker_file: PathBuf,
        slug_dir: PathBuf,
        per_round: Vec<serde_json::Value>,
    ) {
        std::thread::spawn(move || {
            let mut round = 0;
            loop {
                let wrote = pane.writes.lock().unwrap().len();
                if wrote > round {
                    round = wrote;
                    std::fs::create_dir_all(&slug_dir).unwrap();
                    std::thread::sleep(Duration::from_millis(150));
                    // 写"用户文本行"（注入送达确认靠 count_user_inputs 计数它）
                    std::fs::write(
                        &transcript,
                        format!(
                            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"round {round}\"}}}}\n"
                        ),
                    )
                    .unwrap();
                    let marker = per_round
                        .get(round - 1)
                        .cloned()
                        .unwrap_or(serde_json::json!({
                            "session_id": "s1",
                            "transcript_path": transcript.to_string_lossy(),
                        }));
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&marker_file)
                        .unwrap();
                    writeln!(f, "{marker}").unwrap();
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
    }

    fn quick_opts(dir: &Path, rounds: i64) -> EngineOptions {
        EngineOptions {
            task: "写计算器".into(),
            work_dir: dir.to_string_lossy().to_string(),
            max_rounds: rounds,
            silence: Duration::from_millis(400),
            round_timeout: Duration::from_secs(20),
            poll_interval: Duration::from_millis(30),
            delivery_confirm: Duration::from_millis(200),
            first_inject_confirm: Duration::from_millis(0),
            input_ready_wait: Duration::from_millis(0),
            artifacts_dir: None,
            task_token: None,
            reviewer_label: "mock".into(),
            starting_round: 0,
        }
    }

    #[test]
    fn project_slug_matches_real_claude_encoding() {
        // 实测：F:\project\workspace-side\my_skils → F--project-workspace-side-my-skils
        assert_eq!(
            project_slug("F:\\project\\workspace-side\\my_skils"),
            "F--project-workspace-side-my-skils"
        );
        assert_eq!(project_slug("C:\\Work\\My Project"), "C--Work-My-Project");
    }

    #[test]
    fn output_tail_never_splits_utf8() {
        // 中文 TUI 输出是多字节 UTF-8：尾部截断落在字符中间曾直接 panic。
        // 文本不含信任词/就绪标记，两判据都应为假。
        let long = "中文多字节终端输出".repeat(800);
        let tail = output_tail(&long);
        assert!(!tail.is_empty());
        assert!(!looks_like_trust_prompt(&long));
        assert!(!looks_like_input_ready(&long));
        // 截断边界处的字符必须完整（不以不完整 UTF-8 序列开头）
        let first = tail.chars().next().unwrap();
        assert!(tail.starts_with(first));
    }

    #[test]
    fn trust_prompt_is_not_input_ready() {
        assert!(looks_like_trust_prompt(
            "Do you trust the files in this folder?\n> Yes, I trust this folder\n  No, exit"
        ));
        assert!(!looks_like_input_ready(
            "Do you trust the files in this folder?\n> Yes, I trust this folder"
        ));
        assert!(looks_like_input_ready("Claude Code\r\n? for shortcuts"));
        assert!(looks_like_trust_prompt(
            "\u{1b}[32mYes, I trust this folder\u{1b}[0m\nNo, exit"
        ));
        assert!(!looks_like_input_ready(
            "\u{1b}[32mYes, I trust this folder\u{1b}[0m"
        ));
        // 续跑/同 PTY：历史里留着信任文案，但当前已是输入栏 → 不得再当信任菜单
        assert!(!looks_like_trust_prompt(
            "Yes, I trust this folder\nNo, exit\n\nClaude Code\n? for shortcuts"
        ));
        assert!(looks_like_input_ready(
            "Yes, I trust this folder\nNo, exit\n\nClaude Code\n? for shortcuts"
        ));
    }

    #[test]
    fn confirm_inject_keeps_sending_enter_even_if_history_has_trust_text() {
        let pane = FakePane::ok("D:\\work");
        *pane.recent.lock().unwrap() =
            "Do you trust the files in this folder?\nYes, I trust this folder\nNo, exit\n? for shortcuts"
                .into();
        let pane_io: Arc<dyn PaneIo> = pane.clone();
        // confirm_inject 每 2s 才补一次键；窗口略大于 2s 才能观察到写入
        let deadline = Instant::now() + Duration::from_millis(2300);
        let ok = confirm_inject(|| false, &pane_io, deadline, Duration::from_millis(50), None);
        assert!(!ok);
        let writes = pane.writes.lock().unwrap();
        assert!(
            writes.iter().any(|w| w == "\r"),
            "历史含信任文案时仍应补裸回车，不能改发方向键: {writes:?}"
        );
        assert!(
            writes.iter().all(|w| !w.contains("\u{1b}[A")),
            "confirm_inject 不得因历史信任文案发送 TRUST_ACCEPT: {writes:?}"
        );
    }

    #[test]
    fn injects_after_startup_grace_without_shortcut_banner() {
        // 真机：TUI 不一定打出 "? for shortcuts"；不得因此空跑整轮。
        let dir = tmp_dir("splash-then-inject");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("session-1.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let marker_file = dir.join("markers.jsonl");
        std::fs::write(&marker_file, "").unwrap();

        let pane = FakePane::ok(dir.to_string_lossy());
        *pane.recent.lock().unwrap() = "Loading Claude Code…".into();
        simulate_worker(
            pane.clone(),
            transcript,
            marker_file.clone(),
            slug_dir,
            vec![],
        );
        let mut opts = quick_opts(&dir, 1);
        // startup_grace = min(2s, input_ready_wait)；给足宽限后应注入
        opts.input_ready_wait = Duration::from_secs(3);
        let on_log: OnLog = Arc::new(|_: &str| {});
        let src = MarkerSource::new(marker_file);
        let outcome = run(
            &opts,
            pane.clone(),
            Arc::new(MockReviewer::always(Ok(Verdict {
                pass: true,
                reason: "ok".into(),
            }))),
            &src,
            &projects_root,
            &AtomicBool::new(false),
            &on_log,
        );
        assert_eq!(outcome.status, EngineStatus::Accepted);
        let writes = pane.writes.lock().unwrap();
        assert!(
            writes.iter().any(|w| w.contains("写计算器")),
            "无 shortcuts 文案时仍应注入: {writes:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn injects_when_prompt_already_ready() {
        let dir = tmp_dir("prompt-ready");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("session-1.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let marker_file = dir.join("markers.jsonl");
        std::fs::write(&marker_file, "").unwrap();

        let pane = FakePane::ok(dir.to_string_lossy());
        *pane.recent.lock().unwrap() = "? for shortcuts".into();
        simulate_worker(
            pane.clone(),
            transcript,
            marker_file.clone(),
            slug_dir,
            vec![],
        );
        let mut opts = quick_opts(&dir, 1);
        opts.input_ready_wait = Duration::from_secs(3);
        let on_log: OnLog = Arc::new(|_: &str| {});
        let src = MarkerSource::new(marker_file);
        let outcome = run(
            &opts,
            pane.clone(),
            Arc::new(MockReviewer::always(Ok(Verdict {
                pass: true,
                reason: "ok".into(),
            }))),
            &src,
            &projects_root,
            &AtomicBool::new(false),
            &on_log,
        );
        assert_eq!(outcome.status, EngineStatus::Accepted);
        let writes = pane.writes.lock().unwrap();
        assert!(
            writes.iter().any(|w| w.contains("写计算器")),
            "输入栏就绪后应注入: {writes:?}"
        );
        assert!(
            writes.iter().all(|w| !w.contains("\u{1b}[A")),
            "已就绪时不应再打信任菜单方向键: {writes:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn aborts_without_injecting_if_trust_dialog_stays() {
        let dir = tmp_dir("trust-stuck");
        let pane = FakePane::ok(dir.to_string_lossy());
        *pane.recent.lock().unwrap() =
            "Do you trust the files in this folder?\nYes, I trust this folder\nNo, exit".into();
        let mut opts = quick_opts(&dir, 1);
        opts.input_ready_wait = Duration::from_millis(180);
        let on_log: OnLog = Arc::new(|_: &str| {});
        let src = MarkerSource::new(dir.join("none.jsonl"));
        let outcome = run(
            &opts,
            pane.clone(),
            Arc::new(MockReviewer::always(Ok(Verdict {
                pass: true,
                reason: "ok".into(),
            }))),
            &src,
            &dir.join("projects"),
            &AtomicBool::new(false),
            &on_log,
        );
        assert!(matches!(outcome.status, EngineStatus::Aborted(_)), "{:?}", outcome.status);
        let writes = pane.writes.lock().unwrap();
        assert!(
            writes.iter().all(|w| !w.contains("写计算器")),
            "信任菜单未消失时不得注入任务正文: {writes:?}"
        );
        assert!(
            writes.is_empty(),
            "不得自动向信任菜单发键（用户自己选 Yes）: {writes:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn waits_out_trust_dialog_before_injecting_task() {
        let dir = tmp_dir("trust-then-ready");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("session-1.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let marker_file = dir.join("markers.jsonl");
        std::fs::write(&marker_file, "").unwrap();

        let pane = FakePane::ok(dir.to_string_lossy());
        *pane.recent.lock().unwrap() = "Yes, I trust this folder\nNo, exit".into();
        let pane2 = pane.clone();
        // 模拟用户手动选 Yes：信任文案消失
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            *pane2.recent.lock().unwrap() = "? for shortcuts".into();
        });
        simulate_worker(
            pane.clone(),
            transcript,
            marker_file.clone(),
            slug_dir,
            vec![],
        );

        let mut opts = quick_opts(&dir, 1);
        opts.input_ready_wait = Duration::from_secs(4);
        let on_log: OnLog = Arc::new(|_: &str| {});
        let src = MarkerSource::new(marker_file);
        let outcome = run(
            &opts,
            pane.clone(),
            Arc::new(MockReviewer::always(Ok(Verdict {
                pass: true,
                reason: "ok".into(),
            }))),
            &src,
            &projects_root,
            &AtomicBool::new(false),
            &on_log,
        );
        assert_eq!(outcome.status, EngineStatus::Accepted);
        let writes = pane.writes.lock().unwrap();
        assert!(
            writes.iter().any(|w| w.contains("写计算器")),
            "用户选完 Yes 后应注入任务: {writes:?}"
        );
        assert!(
            writes.iter().all(|w| !w.contains("\u{1b}[A")),
            "不得自动发方向键选 Yes: {writes:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_session_containing_picks_token_file() {
        let dir = std::env::temp_dir().join(format!(
            "sv-engine-find-token-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let slug = "proj";
        let slug_dir = dir.join(slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let a = slug_dir.join("sess-a.jsonl");
        let b = slug_dir.join("sess-b.jsonl");
        std::fs::write(&a, "nope\n").unwrap();
        std::fs::write(&b, "hello [supervise-task:task-9] world\n").unwrap();
        let got = find_session_containing(&dir, slug, "[supervise-task:task-9]").unwrap();
        assert_eq!(got, b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_session_containing_does_not_prefix_match_task_ids() {
        let dir = std::env::temp_dir().join(format!(
            "sv-engine-find-token-prefix-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let slug = "proj";
        let slug_dir = dir.join(slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        std::fs::write(slug_dir.join("sess-10.jsonl"), "[supervise-task:task-10]\n").unwrap();
        assert!(
            find_session_containing(&dir, slug, "[supervise-task:task-1]").is_none(),
            "task-10 不得命中 task-1"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 令牌唯一化回归：应用重启后 task_id 从头计数（新旧两次都是 task-2），
    /// 令牌若只带 task_id，旧会话会被新任务误命中（真实事故：二次运行 task-2，
    /// 注入实际落进新会话，引擎却钉死旧会话，审查/送达确认全对着错文件）。
    /// 令牌带 started_at 后，新旧 token 必须互不命中。
    #[test]
    fn find_session_containing_distinguishes_same_task_id_across_runs() {
        let dir = std::env::temp_dir().join(format!(
            "sv-engine-find-token-runs-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let slug = "proj";
        let slug_dir = dir.join(slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let old = slug_dir.join("old-run.jsonl");
        let new = slug_dir.join("new-run.jsonl");
        std::fs::write(&old, "x [supervise-task:task-2:1000] y\n").unwrap();
        std::fs::write(&new, "z [supervise-task:task-2:2000] w\n").unwrap();

        assert_eq!(
            find_session_containing(&dir, slug, "[supervise-task:task-2:2000]"),
            Some(new),
            "新令牌必须只命中新会话"
        );
        assert_eq!(
            find_session_containing(&dir, slug, "[supervise-task:task-2:1000]"),
            Some(old),
            "旧令牌必须只命中旧会话"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn marker_source_only_reads_new_lines() {
        let dir = tmp_dir("markers");
        let marker = dir.join("m.jsonl");
        std::fs::write(&marker, "{\"session_id\":\"old\"}\n").unwrap();
        let src = MarkerSource::new(marker.clone());
        assert!(src.read_new().is_empty(), "启动前的行不算");
        std::fs::write(&marker, "{\"session_id\":\"old\"}\n{\"session_id\":\"new\",\"transcript_path\":\"x\"}\n").unwrap();
        let got = src.read_new();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].session_id.as_deref(), Some("new"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_accepts_after_rework_via_stop_markers() {
        let dir = tmp_dir("accept");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("session-1.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let marker_file = dir.join("markers.jsonl");
        std::fs::write(&marker_file, "").unwrap();

        let pane = FakePane::ok(dir.to_string_lossy());
        simulate_worker(pane.clone(), transcript.clone(), marker_file.clone(), slug_dir.clone(), vec![]);

        // 第 1 轮 REVIEW（缺校验），第 2 轮 PASS
        let reviewer = MockReviewer::scripted(vec![
            Ok(Verdict { pass: false, reason: "缺少输入校验".into() }),
            Ok(Verdict { pass: true, reason: "校验已补齐".into() }),
        ]);
        let cancel = AtomicBool::new(false);
        let logs = Arc::new(Mutex::new(vec![]));
        let logs2 = logs.clone();
        let on_log: OnLog = Arc::new(move |l: &str| logs2.lock().unwrap().push(l.to_string()));

        let src = MarkerSource::new(marker_file);
        let mut opts = quick_opts(&dir, 3);
        opts.artifacts_dir = Some(dir.join(".supervise"));
        std::fs::create_dir_all(dir.join(".supervise")).unwrap();
        // 上一次运行的残留：启动时必须清掉，且不能误删 stop-markers.jsonl
        std::fs::write(dir.join(".supervise").join("review-9.md"), "stale").unwrap();
        std::fs::write(dir.join(".supervise").join("final-report.json"), "stale").unwrap();
        let marker_guard = dir.join(".supervise").join("stop-markers.jsonl");
        std::fs::write(&marker_guard, "keep").unwrap();
        let isolated = dir.join(".supervise").join("tasks").join("task-1");
        std::fs::create_dir_all(&isolated).unwrap();
        let isolated_review = isolated.join("review-1.md");
        std::fs::write(&isolated_review, "keep-headless").unwrap();

        let outcome = run(
            &opts,
            pane.clone(),
            Arc::new(reviewer),
            &src,
            &projects_root,
            &cancel,
            &on_log,
        );

        assert_eq!(outcome.status, EngineStatus::Accepted, "logs: {:?}", logs.lock().unwrap());
        assert_eq!(outcome.rounds, 2, "一次审查失败不应烧轮");
        assert_eq!(outcome.last_reason, "校验已补齐");
        let writes = pane.writes.lock().unwrap();
        // writes[0] 是唤醒回车（防 away 吞输入），任务/返工从 writes[1] 起
        assert!(writes.iter().any(|w| w.contains("写计算器")), "应有任务注入: {writes:?}");
        assert!(
            writes.iter().any(|w| w.contains("缺少输入校验")),
            "应有返工意见注入: {writes:?}"
        );

        // 产物：与 supervise_runner::read_artifacts 契约对齐
        let sup = dir.join(".supervise");
        assert!(!sup.join("review-9.md").exists(), "上次残留必须清理");
        let md1 = std::fs::read_to_string(sup.join("review-1.md")).unwrap();
        assert!(md1.contains("判定：REVIEW"), "{md1}");
        assert!(md1.contains("会话文件："), "{md1}");
        let md2 = std::fs::read_to_string(sup.join("review-2.md")).unwrap();
        assert!(md2.contains("判定：PASS"), "{md2}");
        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(sup.join("final-report.json")).unwrap())
                .unwrap();
        assert_eq!(report["status"], "accepted");
        assert_eq!(report["rounds"], 2);
        assert_eq!(report["verdicts"].as_array().map(Vec::len), Some(2));
        assert!(
            report["verdicts"][0]["file"].as_str().unwrap().ends_with(".jsonl"),
            "verdicts 需带会话文件路径（看板跳转依赖）"
        );
        assert!(marker_guard.exists(), "stop-markers.jsonl 不能被清理误伤");
        assert!(isolated_review.exists(), "引擎 reset 不得删除 tasks/ 子目录");
        assert_eq!(std::fs::read_to_string(&isolated_review).unwrap(), "keep-headless");
        // 跨 crate 契约：看板数据链（read_artifacts）必须能消费引擎产物
        let arts = supervise_runner::read_artifacts(&dir.to_string_lossy(), None).expect("read_artifacts 应解析引擎产物");
        assert_eq!(arts.len(), 2);
        assert_eq!(arts[0].verdict, "REVIEW");
        assert_eq!(arts[1].verdict, "PASS");
        assert!(arts[0].file.ends_with(".jsonl"), "看板跳转依赖 file 字段: {}", arts[0].file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_rejects_when_rounds_exhausted() {
        let dir = tmp_dir("reject");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("s.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let marker_file = dir.join("markers.jsonl");

        let pane = FakePane::ok(dir.to_string_lossy());
        simulate_worker(pane.clone(), transcript, marker_file.clone(), slug_dir, vec![]);

        let reviewer = MockReviewer::always(Ok(Verdict { pass: false, reason: "还是不行".into() }));
        let cancel = AtomicBool::new(false);
        let on_log: OnLog = Arc::new(|_: &str| {});
        let src = MarkerSource::new(marker_file);
        let outcome = run(
            &quick_opts(&dir, 2),
            pane,
            Arc::new(reviewer),
            &src,
            &projects_root,
            &cancel,
            &on_log,
        );
        assert_eq!(outcome.status, EngineStatus::Rejected);
        assert_eq!(outcome.rounds, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_aborts_when_pane_rebound_to_other_dir() {
        let dir = tmp_dir("rebind");
        let pane = FakePane::ok("D:\\somewhere-else");
        let reviewer = MockReviewer::always(Ok(Verdict { pass: true, reason: String::new() }));
        let cancel = AtomicBool::new(false);
        let on_log: OnLog = Arc::new(|_: &str| {});
        let src = MarkerSource::new(dir.join("none.jsonl"));
        let outcome = run(
            &quick_opts(&dir, 3),
            pane.clone(),
            Arc::new(reviewer),
            &src,
            &dir,
            &cancel,
            &on_log,
        );
        assert!(matches!(outcome.status, EngineStatus::Aborted(_)), "{:?}", outcome.status);
        assert!(pane.writes.lock().unwrap().is_empty(), "绑定失效后绝不能注入");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 静默兜底：无 marker 文件，transcript 稳定不动后按静默结束轮次
    #[test]
    fn engine_round_ends_by_silence_without_marker() {
        let dir = tmp_dir("silence");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("s.jsonl");
        std::fs::write(&transcript, "static content\n").unwrap();

        let pane = FakePane::ok(dir.to_string_lossy());
        // 不启动 simulate_worker（不写 marker），静默路径生效
        let reviewer = MockReviewer::always(Ok(Verdict { pass: true, reason: "ok".into() }));
        let cancel = AtomicBool::new(false);
        let on_log: OnLog = Arc::new(|_: &str| {});
        let src = MarkerSource::new(dir.join("no-markers.jsonl"));
        let outcome = run(
            &quick_opts(&dir, 1),
            pane,
            Arc::new(reviewer),
            &src,
            &projects_root,
            &cancel,
            &on_log,
        );
        assert_eq!(outcome.status, EngineStatus::Accepted, "静默兜底应推进到审查并通过");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_aborts_when_reviewer_hard_fails() {
        // Reviewer 内部自带重试，引擎层对硬失败立即中止（不再嵌套重试拖时间）
        let dir = tmp_dir("reviewfail");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("s.jsonl");
        std::fs::write(&transcript, "x\n").unwrap();
        let marker_file = dir.join("markers.jsonl");

        let pane = FakePane::ok(dir.to_string_lossy());
        simulate_worker(pane.clone(), transcript, marker_file.clone(), slug_dir, vec![]);
        let reviewer = MockReviewer::always(Err("codex 挂了".into()));
        let cancel = AtomicBool::new(false);
        let on_log: OnLog = Arc::new(|_: &str| {});
        let src = MarkerSource::new(marker_file);
        let mut opts = quick_opts(&dir, 5);
        opts.artifacts_dir = Some(dir.join(".supervise"));
        let outcome = run(
            &opts,
            pane,
            Arc::new(reviewer),
            &src,
            &projects_root,
            &cancel,
            &on_log,
        );
        assert!(matches!(outcome.status, EngineStatus::Aborted(_)), "{:?}", outcome.status);
        assert_eq!(outcome.rounds, 1, "硬失败应立即中止而非烧完轮次");
        // aborted 的失败原因必须落进最终报告（真实事故：报告只有状态没原因，
        // 用户无从得知为何失败）
        let report: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".supervise").join("final-report.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(report["status"], "aborted");
        assert!(
            report["reason"].as_str().unwrap_or("").contains("codex 挂了"),
            "reason 应带失败原因: {}",
            report["reason"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// C1 回归：第 1 轮的旧 Stop marker 不能在第 2 轮被重复消费。
    /// 第 2 轮 worker 故意延迟写 marker（1.5s > 轮询间隔），若游标缺失，
    /// 第 2 轮会在首次轮询（30ms）时被第 1 轮的旧 marker 立即"结束"。
    #[test]
    fn second_round_ignores_stale_marker_from_round_one() {
        let dir = tmp_dir("stale-marker");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("s.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let marker_file = dir.join("markers.jsonl");
        std::fs::write(&marker_file, "").unwrap();

        let pane = FakePane::ok(dir.to_string_lossy());
        let pane2 = pane.clone();
        let marker2 = marker_file.clone();
        let transcript2 = transcript.clone();
        std::thread::spawn(move || {
            let mut seen_rounds = 0;
            let start = Instant::now();
            loop {
                // 按「每轮正文写」计数（唤醒/提交都是 "\r"），对注入写次数鲁棒，
                // 不因 send_inject 拆分提交键（2 次→3 次写）而把轮次判断写坏
                let writes = pane2.writes.lock().unwrap();
                let text_writes = writes
                    .iter()
                    .filter(|w| !w.is_empty() && w.as_str() != "\r")
                    .count();
                drop(writes);
                if text_writes > seen_rounds {
                    let delay = if seen_rounds == 0 {
                        Duration::from_millis(150) // 第 1 轮：很快写 marker
                    } else {
                        // 第 2 轮：故意延迟 4s——需大于本轮注入的总开销
                        // （提交键 400ms + 交付确认 200ms + 重发唤醒/正文/提交 1.2s
                        //  + 二次确认 200ms ≈ 2s），否则「已注入」日志点到 marker
                        // 落盘的可测窗口被开销吃掉；确保第 2 轮 ≥1.2s 才等到新 marker
                        Duration::from_millis(4000)
                    };
                    seen_rounds = text_writes;
                    std::thread::sleep(delay);
                    std::fs::write(
                        &transcript2,
                        format!(
                            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"round {seen_rounds}\"}}}}\n"
                        ),
                    )
                    .unwrap();
                    // 必须用 json! 序列化：路径反斜杠不转义会产出非法 JSON 被跳过
                    let marker = serde_json::json!({
                        "session_id": "s1",
                        "transcript_path": transcript2.to_string_lossy(),
                    });
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&marker2)
                        .unwrap();
                    writeln!(f, "{marker}").unwrap();
                }
                if start.elapsed() > Duration::from_secs(15) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });

        // 第 1 轮 REVIEW → 注入返工 → 第 2 轮必须等新 marker（≥1.5s）才结束
        let reviewer = MockReviewer::scripted(vec![
            Ok(Verdict { pass: false, reason: "缺校验".into() }),
            Ok(Verdict { pass: true, reason: "已补".into() }),
        ]);
        let cancel = AtomicBool::new(false);
        let round2_start = Arc::new(Mutex::new(None::<Instant>));
        let round2_end = Arc::new(Mutex::new(None::<Instant>));
        let round2_by_marker = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let all_logs = Arc::new(Mutex::new(vec![]));
        let on_log: OnLog = {
            let all_logs = all_logs.clone();
            let round2_start = round2_start.clone();
            let round2_end = round2_end.clone();
            let round2_by_marker = round2_by_marker.clone();
            Arc::new(move |l: &str| {
                all_logs.lock().unwrap().push(l.to_string());
                if l.contains("第 2/2 轮已注入") {
                    *round2_start.lock().unwrap() = Some(Instant::now());
                }
                if l.contains("第 2 轮结束") {
                    *round2_end.lock().unwrap() = Some(Instant::now());
                    if l.contains("Stop hook 信号") {
                        round2_by_marker.store(true, Ordering::Relaxed);
                    }
                }
            })
        };

        // 静默阈值调大到 3s：第 2 轮（marker 延迟 1.5s 写入）必须靠新 marker
        // 结束，而不是 400ms 的静默兜底抢先——这样才真正验证游标行为
        let mut opts = quick_opts(&dir, 2);
        opts.silence = Duration::from_secs(3);

        let src = MarkerSource::new(marker_file);
        let outcome = run(
            &opts,
            pane.clone(),
            Arc::new(reviewer),
            &src,
            &projects_root,
            &cancel,
            &on_log,
        );

        assert_eq!(outcome.status, EngineStatus::Accepted);
        assert!(
            round2_by_marker.load(Ordering::Relaxed),
            "第 2 轮应等到新 marker（Stop hook 信号）才结束，而非旧 marker 短路或静默兜底"
        );
        let start = round2_start.lock().unwrap();
        let end = round2_end.lock().unwrap();
        let duration = end.zip(start.as_ref().copied()).map(|(e, s)| e - s);
        assert!(
            duration.is_some_and(|d| d >= Duration::from_millis(1200)),
            "第 2 轮耗时应 ≥ 1.2s（等新 marker），实际 {duration:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 首轮注入重发回归：首次注入的令牌没落盘（TUI 未就绪/注入被吞，需手动回车才能
    /// 执行的场景）时，引擎应在确认窗口后重发注入，而不是直接「未能按令牌预钉」中止；
    /// 重发后令牌落盘 → 正常预钉并推进。修复前：首次注入不见令牌即中止。
    #[test]
    fn first_round_resends_injection_when_token_not_landed() {
        let dir = tmp_dir("first-round-resend");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("session-1.jsonl");
        let marker_file = dir.join("markers.jsonl");
        std::fs::write(&marker_file, "").unwrap();

        let pane = FakePane::ok(dir.to_string_lossy());
        let pane2 = pane.clone();
        let marker2 = marker_file.clone();
        let transcript2 = transcript.clone();
        let token = "[supervise-task:task-x]".to_string();
        std::thread::spawn(move || {
            let mut responded = false;
            let start = Instant::now();
            loop {
                let writes = pane2.writes.lock().unwrap();
                let text_writes = writes
                    .iter()
                    .filter(|w| !w.is_empty() && w.as_str() != "\r")
                    .count();
                drop(writes);
                // 第二次注入（重发）发出后，才把令牌写进 transcript + 写 marker：
                // 首轮第一次注入期间令牌不可见，模拟 TUI 未就绪吞掉首次注入
                if !responded && text_writes >= 2 {
                    std::fs::create_dir_all(&slug_dir).unwrap();
                    std::fs::write(
                        &transcript2,
                        format!(
                            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{token} 任务开始\"}}}}\n"
                        ),
                    )
                    .unwrap();
                    use std::io::Write;
                    let marker = serde_json::json!({
                        "session_id": "s1",
                        "transcript_path": transcript2.to_string_lossy(),
                    });
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&marker2)
                        .unwrap();
                    writeln!(f, "{marker}").unwrap();
                    responded = true;
                    std::thread::sleep(Duration::from_millis(30));
                    continue;
                }
                if start.elapsed() > Duration::from_secs(25) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });

        let reviewer = MockReviewer::always(Ok(Verdict { pass: true, reason: "ok".into() }));
        let cancel = AtomicBool::new(false);
        let all_logs = Arc::new(Mutex::new(vec![]));
        let logs2 = all_logs.clone();
        let on_log: OnLog = Arc::new(move |l: &str| logs2.lock().unwrap().push(l.to_string()));
        let src = MarkerSource::new(marker_file);
        let mut opts = quick_opts(&dir, 2);
        opts.task_token = Some("task-x".into());

        let outcome = run(
            &opts,
            pane.clone(),
            Arc::new(reviewer),
            &src,
            &projects_root,
            &cancel,
            &on_log,
        );

        assert_eq!(
            outcome.status,
            EngineStatus::Accepted,
            "首轮重发后应正常推进而非中止；logs: {:?}",
            all_logs.lock().unwrap()
        );
        assert!(
            all_logs
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("令牌未落盘，重发首轮注入")),
            "应重发首轮注入: {:?}",
            all_logs.lock().unwrap()
        );
        let writes = pane.writes.lock().unwrap();
        let text_writes = writes
            .iter()
            .filter(|w| !w.is_empty() && w.as_str() != "\r")
            .count();
        assert_eq!(text_writes, 2, "应注入两次（首+重发）: {writes:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 「再来一轮」续跑回归：starting_round 使轮次从原轮数继续（原 3 轮 →
    /// 第 4/4 轮），首轮注入为续跑返工文本，通过后 rounds=4（而非从 1 重数）。
    #[test]
    fn continuation_offsets_round_count_from_starting_round() {
        let dir = tmp_dir("continue");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("session-1.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let marker_file = dir.join("markers.jsonl");
        std::fs::write(&marker_file, "").unwrap();

        let pane = FakePane::ok(dir.to_string_lossy());
        let pane2 = pane.clone();
        let transcript2 = transcript.clone();
        let marker2 = marker_file.clone();
        std::thread::spawn(move || {
            let mut seen = 0;
            let start = Instant::now();
            loop {
                let writes = pane2.writes.lock().unwrap().len();
                if writes > seen {
                    seen = writes;
                    std::thread::sleep(Duration::from_millis(150));
                    std::fs::write(
                        &transcript2,
                        format!(
                            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"round {seen}\"}}}}\n"
                        ),
                    )
                    .unwrap();
                    let marker = serde_json::json!({
                        "session_id": "s1",
                        "transcript_path": transcript2.to_string_lossy(),
                    });
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&marker2)
                        .unwrap();
                    writeln!(f, "{marker}").unwrap();
                }
                if start.elapsed() > Duration::from_secs(20) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });

        let reviewer = MockReviewer::always(Ok(Verdict { pass: true, reason: "ok".into() }));
        let cancel = AtomicBool::new(false);
        let all_logs = Arc::new(Mutex::new(vec![]));
        let logs2 = all_logs.clone();
        let on_log: OnLog = Arc::new(move |l: &str| logs2.lock().unwrap().push(l.to_string()));
        let src = MarkerSource::new(marker_file);
        let mut opts = quick_opts(&dir, 4); // max_rounds = 原 3 + 续 1
        opts.starting_round = 3; // 原 3 轮后「再来一轮」
        opts.task = "上一轮审查未通过，请按要求返工：xxx".into();

        let outcome = run(
            &opts,
            pane.clone(),
            Arc::new(reviewer),
            &src,
            &projects_root,
            &cancel,
            &on_log,
        );

        assert_eq!(outcome.status, EngineStatus::Accepted);
        assert_eq!(outcome.rounds, 4, "续跑轮次应从 3 继续到 4: {outcome:?}");
        assert!(
            all_logs
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("第 4/4 轮已注入")),
            "日志应显示第 4/4 轮: {:?}",
            all_logs.lock().unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// V1+V2 联合回归：第 2 轮工人不响应（注入被终端吞掉的场景）——
    /// 复读守卫不得再调审查（杜绝"复读假 PASS"），注入确认失败须重发一次。
    /// 复刻真实事故：工人未动、审查者复读同一份会话却放行 PASS
    #[test]
    fn unchanged_transcript_round_skips_reviewer_and_retries_injection() {
        let dir = tmp_dir("unchanged");
        let slug = project_slug(&dir.to_string_lossy());
        let projects_root = dir.join("projects");
        let slug_dir = projects_root.join(&slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let transcript = slug_dir.join("s.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"种子\"}}\n",
        )
        .unwrap();
        let marker_file = dir.join("markers.jsonl");

        let pane = FakePane::ok(dir.to_string_lossy());
        // worker：只响应第一次注入（写一次用户行 + marker），此后沉默——
        // 模拟第 2 轮注入被终端空闲状态吞掉
        {
            let pane2 = pane.clone();
            let t2 = transcript.clone();
            let m2 = marker_file.clone();
            std::thread::spawn(move || {
                let mut fired = false;
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(15) {
                    let writes = pane2.writes.lock().unwrap().len();
                    if !fired && writes >= 2 {
                        fired = true;
                        std::thread::sleep(Duration::from_millis(100));
                        std::fs::write(
                            &t2,
                            concat!(
                                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"种子\"}}\n",
                                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"round 1\"}}\n"
                            ),
                        )
                        .unwrap();
                        let marker = serde_json::json!({
                            "session_id": "s1",
                            "transcript_path": t2.to_string_lossy(),
                        });
                        use std::io::Write;
                        let mut f = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&m2)
                            .unwrap();
                        writeln!(f, "{marker}").unwrap();
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
        }

        // 计数审查器：内层脚本化为 REVIEW（若引擎错误地复读审查，计数会 >1
        // 且理由是"缺校验"而非"无变化"）
        struct CountingReviewer {
            inner: MockReviewer,
            calls: std::sync::atomic::AtomicUsize,
        }
        impl Reviewer for CountingReviewer {
            fn review(
                &self,
                t: &Path,
                r: i64,
                c: &AtomicBool,
            ) -> Result<Verdict, String> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                self.inner.review(t, r, c)
            }
        }
        let reviewer = Arc::new(CountingReviewer {
            inner: MockReviewer::scripted(vec![Ok(Verdict {
                pass: false,
                reason: "缺校验".into(),
            })]),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });

        let cancel = AtomicBool::new(false);
        let on_log: OnLog = Arc::new(|_: &str| {});
        let src = MarkerSource::new(marker_file);
        let outcome = run(
            &quick_opts(&dir, 2),
            pane.clone(),
            reviewer.clone(),
            &src,
            &projects_root,
            &cancel,
            &on_log,
        );

        assert_eq!(outcome.status, EngineStatus::Rejected, "{:?}", outcome.status);
        assert_eq!(
            reviewer.calls.load(Ordering::Relaxed),
            1,
            "会话无变化的轮次必须跳过审查（复读=假 PASS 温床）"
        );
        assert!(
            outcome.last_reason.contains("无任何变化"),
            "应以'会话无变化'收尾而非审查者理由: {}",
            outcome.last_reason
        );
        let rework_sends = pane
            .writes
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w.contains("请按要求返工"))
            .count();
        assert!(
            rework_sends >= 2,
            "注入确认失败应重发一次（V2），实际返工注入 {rework_sends} 次"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
