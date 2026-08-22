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

// ---------------- 引擎契约 ----------------

/// 终端 pane 的 IO 通道（lib.rs 用 TerminalState 的 PTY writer 实现）
pub trait PaneIo: Send + Sync {
    /// 注入文本到 pane（引擎负责追加回车）
    fn write(&self, data: &str) -> Result<(), String>;
    /// pane 当前绑定的工作目录（会话被停掉/替换/目录不符时返回 None 或不等的值，
    /// 引擎据此中止——防止意见注入到错误目录的会话）
    fn current_work_dir(&self) -> Option<String>;
}

/// 日志回调（lib.rs 转 supervise-log 事件）
pub type OnLog = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub task: String,
    /// pane 必须绑定的目录（绑定校验用，注入前逐轮检查）
    pub work_dir: String,
    pub max_rounds: i64,
    /// JSONL 静默兜底阈值（默认 120s：工具执行期间文件不动是常态）
    pub silence: Duration,
    /// 单轮硬超时（默认 15min）
    pub round_timeout: Duration,
    pub poll_interval: Duration,
    /// 产物目录（.supervise）：逐轮 review-N.md + 结束 final-report.json。
    /// None 则不落盘（纯测试用）。注意只清 review-*.md/final-report.json，
    /// 不能整目录删——stop-markers.jsonl 也在这里，MarkerSource 正快照着它
    pub artifacts_dir: Option<PathBuf>,
    /// 审查者标签（写产物用：模型名 / "mock"）
    pub reviewer_label: String,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            task: String::new(),
            work_dir: String::new(),
            max_rounds: 3,
            silence: Duration::from_secs(120),
            round_timeout: Duration::from_secs(15 * 60),
            poll_interval: Duration::from_millis(500),
            artifacts_dir: None,
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

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| {
            let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
            // PS5.1 的 Add-Content -Encoding UTF8 首次建文件带 BOM，不剥掉会
            // 让首个 marker 的 JSON 解析失败、第一轮丢失主信号
            if let Some(first) = lines.first_mut() {
                *first = first.trim_start_matches('\u{feff}').to_string();
            }
            lines
        })
        .unwrap_or_default()
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
        // 兜底定位：slug 目录里引擎启动后活跃的会话文件
        if transcript.is_none() {
            transcript = newest_session_after(projects_root, slug, cutoff);
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

    let mut last_reason = String::new();
    let mut first_inject = true;
    let mut session_pin: Option<String> = None;
    let mut round: i64 = 0;
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
        // 不重新注入（见下方审查重试循环），first_inject 只认第一次真正注入
        let inject = if first_inject {
            format!("{}\r", opts.task)
        } else {
            format!("上一轮审查未通过，请按要求返工：{}\r", last_reason)
        };
        first_inject = false;
        if let Err(e) = pane.write(&inject) {
            let msg = format!("注入失败：{e}");
            on_log(&format!("[ENGINE] {msg}"));
            return EngineOutcome {
                status: EngineStatus::Aborted(msg),
                rounds: round - 1,
                last_reason,
            };
        }
        on_log(&format!(
            "[ENGINE] 第 {round}/{} 轮已注入，等待干活完成…",
            opts.max_rounds
        ));

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
                    std::fs::write(
                        &transcript,
                        format!("{{\"type\":\"user\",\"message\":{{}}}}\nround {round}\n"),
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
            artifacts_dir: None,
            reviewer_label: "mock".into(),
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

        let pane = Arc::new(FakePane {
            dir_ok: Mutex::new(Some(dir.to_string_lossy().to_string())),
            writes: Mutex::new(vec![]),
            fail_write: AtomicBool::new(false),
        });
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
        assert!(writes[0].contains("写计算器"), "首轮注入任务: {}", writes[0]);
        assert!(writes[1].contains("缺少输入校验"), "次轮注入返工意见: {}", writes[1]);

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
        // 跨 crate 契约：看板数据链（read_artifacts）必须能消费引擎产物
        let arts = supervise_runner::read_artifacts(&dir.to_string_lossy()).expect("read_artifacts 应解析引擎产物");
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

        let pane = Arc::new(FakePane {
            dir_ok: Mutex::new(Some(dir.to_string_lossy().to_string())),
            writes: Mutex::new(vec![]),
            fail_write: AtomicBool::new(false),
        });
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
        let pane = Arc::new(FakePane {
            dir_ok: Mutex::new(Some("D:\\somewhere-else".into())),
            writes: Mutex::new(vec![]),
            fail_write: AtomicBool::new(false),
        });
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

        let pane = Arc::new(FakePane {
            dir_ok: Mutex::new(Some(dir.to_string_lossy().to_string())),
            writes: Mutex::new(vec![]),
            fail_write: AtomicBool::new(false),
        });
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

        let pane = Arc::new(FakePane {
            dir_ok: Mutex::new(Some(dir.to_string_lossy().to_string())),
            writes: Mutex::new(vec![]),
            fail_write: AtomicBool::new(false),
        });
        simulate_worker(pane.clone(), transcript, marker_file.clone(), slug_dir, vec![]);
        let reviewer = MockReviewer::always(Err("codex 挂了".into()));
        let cancel = AtomicBool::new(false);
        let on_log: OnLog = Arc::new(|_: &str| {});
        let src = MarkerSource::new(marker_file);
        let outcome = run(
            &quick_opts(&dir, 5),
            pane,
            Arc::new(reviewer),
            &src,
            &projects_root,
            &cancel,
            &on_log,
        );
        assert!(matches!(outcome.status, EngineStatus::Aborted(_)), "{:?}", outcome.status);
        assert_eq!(outcome.rounds, 1, "硬失败应立即中止而非烧完轮次");
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

        let pane = Arc::new(FakePane {
            dir_ok: Mutex::new(Some(dir.to_string_lossy().to_string())),
            writes: Mutex::new(vec![]),
            fail_write: AtomicBool::new(false),
        });
        let pane2 = pane.clone();
        let marker2 = marker_file.clone();
        let transcript2 = transcript.clone();
        std::thread::spawn(move || {
            let mut seen_rounds = 0;
            let start = Instant::now();
            loop {
                let writes = pane2.writes.lock().unwrap().len();
                if writes > seen_rounds {
                    let delay = if seen_rounds == 0 {
                        Duration::from_millis(150) // 第 1 轮：很快写 marker
                    } else {
                        Duration::from_millis(1500) // 第 2 轮：故意延迟
                    };
                    seen_rounds = writes;
                    std::thread::sleep(delay);
                    std::fs::write(&transcript2, format!("round {seen_rounds}\n")).unwrap();
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
            "第 2 轮应等到新 marker（Stop hook 信号）才结束，而非旧 marker 短路或静默兜底；日志: {:?}",
            all_logs.lock().unwrap()
        );
        // 第 2 轮从注入到结束必须 ≥ 1.2s（worker 延迟 1.5s 写新 marker），
        // 旧 marker 若被重复消费会在首次轮询（30ms）内短路
        let start = round2_start.lock().unwrap();
        let end = round2_end.lock().unwrap();
        let duration = end.zip(start.as_ref().copied()).map(|(e, s)| e - s);
        assert!(
            duration.is_some_and(|d| d >= Duration::from_millis(1200)),
            "第 2 轮耗时应 ≥ 1.2s（等新 marker），实际 {duration:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
