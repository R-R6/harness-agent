//! supervise_runner —— 监督闭环进程桥
//!
//! 复用内置的 supervise.ps1（真实闭环引擎，随应用打包在
//! src-tauri/resources/supervise-loop-script/），本 crate 负责：
//! 1. spawn pwsh supervise.ps1（参数映射）
//! 2. 解析 .supervise 产物（review-N.md 逐轮意见 + final-report.json 最终报告）
//!
//! 独立 crate 原因同 session_proxy：不依赖 tauri，cargo test 可正常跑
//! （tauri 依赖链会引入 WebView2Loader.dll 运行时问题）。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

// ---------------- 请求/产物数据结构（契约） ----------------

/// 启动监督闭环的请求参数（对应 supervise.ps1 参数）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuperviseRequest {
    pub task: String,
    pub work_dir: String,
    /// L0/L1/L2（默认 L1，映射 MaxRounds 1/3/5 + 模型）
    #[serde(default)]
    pub level: Option<String>,
    /// 显式轮数（>0 时覆盖 Level 推导）
    #[serde(default)]
    pub max_rounds: Option<i64>,
    /// 显式审查模型（非空时覆盖 Level 推导）
    #[serde(default)]
    pub model: Option<String>,
    /// 模拟模式（不真调 claude/codex）
    #[serde(default)]
    pub mock: bool,
}

/// final-report.json 里的单轮审查记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictEntry {
    pub round: i64,
    /// PASS / REVIEW
    pub verdict: String,
    pub reason: String,
    #[serde(rename = "sessionId", default)]
    pub session_id: String,
    #[serde(default)]
    pub file: String,
}

/// final-report.json 最终报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuperviseSummary {
    /// accepted（通过）/ rejected（轮数用完未过）
    pub status: String,
    pub task: String,
    pub rounds: i64,
    #[serde(rename = "sessionId", default)]
    pub session_id: String,
    #[serde(rename = "sessionFile", default)]
    pub session_file: String,
    pub verdicts: Vec<VerdictEntry>,
}

/// 单轮审查意见（review-N.md 解析结果）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewArtifact {
    pub round: i64,
    pub verdict: String,
    pub reason: String,
    pub model: String,
    pub session_id: String,
    /// 会话 JSONL 完整路径（final-report 携带、review-N.md 的「会话文件：」行
    /// 提取；旧产物无此信息时为空——前端据此禁用跳转）
    #[serde(default)]
    pub file: String,
}

// ---------------- 路径定位 ----------------

/// PowerShell 可执行文件：优先环境变量 HARNESS_PWSH，否则 powershell（Windows 自带 5.1）
fn pwsh_path() -> String {
    std::env::var("HARNESS_PWSH").unwrap_or_else(|_| "powershell".to_string())
}

/// supervise.ps1 路径（三级解析）：
/// 1. 环境变量 HARNESS_SUPERVISE_SCRIPT（测试/调试覆盖）
/// 2. tauri setup 注入的打包资源路径（发布/开发）
/// 3. 兜底：仓库内副本（cargo test 无注入时的默认值）
static SUPERVISE_SCRIPT: OnceLock<PathBuf> = OnceLock::new();

/// 由 tauri 启动时注入打包资源目录里的真实 supervise.ps1 路径
pub fn set_supervise_script(path: PathBuf) {
    let _ = SUPERVISE_SCRIPT.set(path);
}

fn supervise_script_path() -> PathBuf {
    path_util::resolve_script(
        "HARNESS_SUPERVISE_SCRIPT",
        SUPERVISE_SCRIPT.get(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/supervise-loop-script/supervise.ps1"
        ),
    )
}

// ---------------- 进程桥 ----------------

/// spawn pwsh supervise.ps1（stdout/stderr 用管道，由调用方异步读取；stdin 关闭）
pub fn spawn_supervise(req: &SuperviseRequest) -> Result<Child, String> {
    let mut cmd = Command::new(pwsh_path());
    cmd.arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(supervise_script_path())
        .arg("-Task")
        .arg(&req.task)
        .arg("-WorkDir")
        .arg(&req.work_dir);

    if req.mock {
        cmd.arg("-Mock");
    }
    if let Some(l) = &req.level {
        cmd.arg("-Level").arg(l);
    }
    if let Some(m) = req.max_rounds {
        if m > 0 {
            cmd.arg("-MaxRounds").arg(m.to_string());
        }
    }
    if let Some(m) = &req.model {
        if !m.is_empty() {
            cmd.arg("-Model").arg(m);
        }
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    cmd.spawn()
        .map_err(|e| format!("spawn pwsh 失败（请确认 PowerShell 可用）: {e}"))
}

/// 后台线程消费子进程 stderr，逐行回调。
/// stderr 管道若无人读取，子进程写满系统管道缓冲（Windows 4-64KB）后会阻塞在
/// stderr 写入上——整个监督任务挂死、busy_workdirs 永不清理、该目录被永久锁死。
pub fn drain_stderr<F>(stderr: std::process::ChildStderr, on_line: F)
where
    F: Fn(String) + Send + 'static,
{
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr).lines() {
            if let Ok(l) = line {
                on_line(l);
            }
        }
    });
}

// ---------------- 产物解析 ----------------

/// 读 .supervise 目录产物：
/// 1. final-report.json（如果有）→ 结构化摘要
/// 2. review-N.md 系列 → 逐轮意见
pub fn read_artifacts(work_dir: &str) -> Result<Vec<ReviewArtifact>, String> {
    let dir = Path::new(work_dir).join(".supervise");
    if !dir.exists() {
        return Ok(vec![]);
    }

    // 优先 final-report.json 的 verdicts（结构化、完整）
    let report_path = dir.join("final-report.json");
    if report_path.exists() {
        if let Ok(text) = std::fs::read_to_string(&report_path) {
            if let Ok(summary) = serde_json::from_str::<SuperviseSummary>(&text) {
                let artifacts: Vec<ReviewArtifact> = summary
                    .verdicts
                    .iter()
                    .map(|v| ReviewArtifact {
                        round: v.round,
                        verdict: v.verdict.clone(),
                        reason: v.reason.clone(),
                        model: String::new(),
                        session_id: v.session_id.clone(),
                        file: v.file.clone(),
                    })
                    .collect();
                if !artifacts.is_empty() {
                    return Ok(artifacts);
                }
            }
        }
    }

    // fallback：逐轮解析 review-N.md
    let mut out = vec![];
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("读 .supervise 失败: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("review-") && n.ends_with(".md"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();

    for f in files {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Some(a) = parse_review_md(&text) {
                out.push(a);
            }
        }
    }
    Ok(out)
}

/// 解析 review-N.md 文本 → ReviewArtifact（容错：解析失败返回 None）
fn parse_review_md(text: &str) -> Option<ReviewArtifact> {
    // supervise.ps1 写 md 固定带 UTF-8 BOM（UTF8Encoding($true)）；不剥掉则
    // 首行 "# 第 N 轮" 被 BOM 顶住，starts_with 匹配失败、整个解析返回 None
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let round = text
        .lines()
        .find(|l| l.starts_with("# 第"))
        .and_then(|l| {
            l.split(|c: char| !c.is_ascii_digit())
                .find(|s| !s.is_empty())
        })
        .and_then(|s| s.parse::<i64>().ok())?;

    let verdict = text
        .lines()
        .find(|l| l.contains("判定"))
        .and_then(|l| l.split(['：', ':']).nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let model = text
        .lines()
        .find(|l| l.contains("模型"))
        .and_then(|l| l.split(['：', ':']).nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let session_id = text
        .lines()
        .find(|l| l.contains("会话："))
        .and_then(|l| l.split(['：', ':']).nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    // 会话 JSONL 完整路径（新版产物携带；「会话：」行不含、注意与 sessionId 行区分）。
    // 只按第一个冒号切分：Windows 路径自带 "C:" 冒号，多分隔符切分会截断成盘符
    let file = text
        .lines()
        .find(|l| l.contains("会话文件"))
        .and_then(|l| {
            l.split_once('：')
                .or_else(|| l.split_once(':'))
                .map(|(_, rest)| rest.trim().to_string())
        })
        .unwrap_or_default();

    // reason：## 意见 之后的所有行
    let reason = text
        .split("## 意见")
        .nth(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    Some(ReviewArtifact {
        round,
        verdict,
        reason,
        model,
        session_id,
        file,
    })
}

// ---------------- 测试 ----------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 产物解析 ----

    #[test]
    fn parse_review_md_extracts_fields() {
        let md = "# 第 3 轮审查意见\n\n- 判定：REVIEW\n- 审查模型：gpt-5.6-luna\n- 会话：mock-0001\n- 会话文件：C:\\Users\\u\\.claude\\projects\\p\\s.jsonl\n\n## 意见\n\n缺少输入校验，请补充。\n";
        let a = parse_review_md(md).expect("应解析成功");
        assert_eq!(a.round, 3);
        assert_eq!(a.verdict, "REVIEW");
        assert_eq!(a.model, "gpt-5.6-luna");
        assert_eq!(a.session_id, "mock-0001");
        assert_eq!(
            a.file, "C:\\Users\\u\\.claude\\projects\\p\\s.jsonl",
            "会话文件行应提取完整路径"
        );
        assert!(a.reason.contains("缺少输入校验"));
    }

    /// 旧版产物没有「会话文件：」行：file 为空（前端据此禁用跳转），其余字段正常
    #[test]
    fn parse_review_md_old_artifact_has_empty_file() {
        let md = "# 第 1 轮审查意见\n\n- 判定：PASS\n- 会话：mock-1\n\n## 意见\n\n一次通过。\n";
        let a = parse_review_md(md).expect("应解析成功");
        assert_eq!(a.file, "");
        assert_eq!(a.session_id, "mock-1");
        assert_eq!(a.verdict, "PASS");
    }

    #[test]
    fn read_artifacts_returns_empty_when_no_dir() {
        let tmp = std::env::temp_dir().join(format!("sv-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(read_artifacts(&tmp.to_string_lossy()).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn read_artifacts_parses_final_report_json() {
        let tmp = std::env::temp_dir().join(format!("sv-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let sup = tmp.join(".supervise");
        std::fs::create_dir_all(&sup).unwrap();
        std::fs::write(
            sup.join("final-report.json"),
            r#"{"status":"accepted","task":"写计算器","rounds":2,"sessionId":"s1","sessionFile":"f1","verdicts":[{"round":1,"verdict":"REVIEW","reason":"缺校验","sessionId":"s1","file":"f1"},{"round":2,"verdict":"PASS","reason":"已补","sessionId":"s1","file":"f1"}]}"#,
        )
        .unwrap();
        let arts = read_artifacts(&tmp.to_string_lossy()).expect("解析成功");
        assert_eq!(arts.len(), 2);
        assert_eq!(arts[0].verdict, "REVIEW");
        assert_eq!(arts[1].verdict, "PASS");
        assert_eq!(arts[1].round, 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn read_artifacts_falls_back_to_review_md() {
        let tmp = std::env::temp_dir().join(format!("sv-md-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let sup = tmp.join(".supervise");
        std::fs::create_dir_all(&sup).unwrap();
        // fixture 带 UTF-8 BOM，与 supervise.ps1 的真实产物一致（UTF8Encoding($true)）
        std::fs::write(
            sup.join("review-1.md"),
            "\u{feff}# 第 1 轮审查意见\n\n- 判定：PASS\n- 审查模型：gpt-5.6-luna\n- 会话：mock-9\n- 会话文件：C:\\s\\mock-9.jsonl\n\n## 意见\n\n一次通过。\n",
        )
        .unwrap();
        let arts = read_artifacts(&tmp.to_string_lossy()).expect("解析成功");
        assert_eq!(arts.len(), 1);
        assert_eq!(arts[0].verdict, "PASS");
        assert_eq!(arts[0].session_id, "mock-9");
        assert_eq!(arts[0].file, "C:\\s\\mock-9.jsonl");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// supervise.ps1 的 md 产物固定带 BOM；不剥掉时首行 "# 第 N 轮" 匹配失败，
    /// final-report.json 缺失（脚本中途崩溃）时兜底解析会整体失效
    #[test]
    fn parse_review_md_strips_bom_from_real_artifact() {
        let md = "\u{feff}# 第 2 轮审查意见\n\n- 判定：REVIEW\n- 审查模型：gpt-5.6-luna\n- 会话：mock-2\n\n## 意见\n\n缺少测试。\n";
        let a = parse_review_md(md).expect("带 BOM 的真实产物必须可解析");
        assert_eq!(a.round, 2);
        assert_eq!(a.verdict, "REVIEW");
        assert!(a.reason.contains("缺少测试"));
    }

    // ---- 进程桥（fake ps1 fixture） ----

    #[test]
    fn spawn_supervise_passes_args_and_captures_stdout() {
        // 用假的 ps1 充当 supervise 脚本：打印收到的参数 + 输出模拟日志
        let tmp = std::env::temp_dir().join(format!("sv-spawn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let fake_script = tmp.join("fake-supervise.ps1");
        // fake 脚本必须与真实 supervise.ps1 契约对齐：stdout 走 UTF-8（真实脚本
        // 头部已强制 [Console]::OutputEncoding=UTF8）。缺这行中文会按 OEM 代码页
        // 936 编码，Rust 侧 UTF-8 解码失败、中文行丢失
        std::fs::write(
            &fake_script,
            "param([string]$Task,[string]$WorkDir,[switch]$Mock,[string]$Level)\n\
             try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false) } catch {}\n\
             Write-Output \"TASK=$Task\"\n\
             Write-Output \"WORKDIR=$WorkDir\"\n\
             Write-Output \"MOCK=$Mock\"\n\
             Write-Output \"LEVEL=$Level\"\n",
        )
        .unwrap();

        // 环境变量指向 fake 脚本；HARNESS_PWSH 用 powershell.exe
        let old_script = std::env::var("HARNESS_SUPERVISE_SCRIPT").ok();
        std::env::set_var("HARNESS_SUPERVISE_SCRIPT", &fake_script);
        std::env::set_var("HARNESS_PWSH", "powershell");

        let req = SuperviseRequest {
            task: "写计算器".into(),
            work_dir: "D:\\work".into(),
            level: Some("L2".into()),
            max_rounds: None,
            model: None,
            mock: true,
        };
        let mut child = spawn_supervise(&req).expect("spawn 成功");

        // 读 stdout（子进程退出后读完）
        use std::io::{BufRead, BufReader};
        let stdout = child.stdout.take().expect("stdout 管道");
        let out: String = BufReader::new(stdout)
            .lines()
            .map(|l| l.unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = child.wait();

        assert!(out.contains("TASK=写计算器"), "含 Task: {out}");
        assert!(out.contains("WORKDIR=D:\\work"), "含 WorkDir: {out}");
        assert!(out.contains("LEVEL=L2"), "含 Level: {out}");
        // Mock 是 switch：传了 -Mock 时输出 "MOCK=True"
        assert!(out.contains("MOCK=True"), "含 Mock: {out}");

        match old_script {
            Some(v) => std::env::set_var("HARNESS_SUPERVISE_SCRIPT", v),
            None => std::env::remove_var("HARNESS_SUPERVISE_SCRIPT"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// stderr 管道必须有人消费：子进程写超过管道缓冲（4-64KB）的 stderr 时，
    /// 无人读取会导致子进程阻塞在写入上永远不退出。drain_stderr 消费后子进程
    /// 正常退出且逐行回调不丢首尾行。
    #[test]
    fn drain_stderr_consumes_more_than_pipe_buffer() {
        let tmp = std::env::temp_dir().join(format!("sv-errdrain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // 8192 行 × ~16 字节 ≈ 128KB，远超 Windows 管道缓冲
        let fake_script = tmp.join("stderr-flood.ps1");
        std::fs::write(
            &fake_script,
            "param()\n\
             try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false) } catch {}\n\
             for ($i = 1; $i -le 8192; $i++) { [Console]::Error.WriteLine(\"ERR-$i-padpadpadpad\") }\n\
             Write-Output \"DONE\"\n",
        )
        .unwrap();

        let old_script = std::env::var("HARNESS_SUPERVISE_SCRIPT").ok();
        std::env::set_var("HARNESS_SUPERVISE_SCRIPT", &fake_script);
        std::env::set_var("HARNESS_PWSH", "powershell");

        let req = SuperviseRequest {
            task: "t".into(),
            work_dir: "D:\\work".into(),
            level: None,
            max_rounds: None,
            model: None,
            mock: true,
        };
        let mut child = spawn_supervise(&req).expect("spawn 成功");
        let stderr = child.stderr.take().expect("stderr 管道");
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        drain_stderr(stderr, move |line| {
            let _ = tx.send(line);
        });

        // 只读 stdout（不读 stderr）：若 drain_stderr 没在消费，子进程会在写满
        // 管道缓冲后阻塞，stdout 永远等不到 DONE，测试在此超时
        use std::io::BufRead;
        let stdout = child.stdout.take().expect("stdout 管道");
        let saw_done = std::io::BufReader::new(stdout)
            .lines()
            .any(|l| l.map(|l| l.contains("DONE")).unwrap_or(false));
        let _ = child.wait();

        // wait 返回时 drain 线程可能还在收尾，用 recv_timeout 等它读完管道余量
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut count = 0usize;
        let mut first = String::new();
        let mut last = String::new();
        while count < 8192 {
            match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(line) => {
                    if count == 0 {
                        first = line.clone();
                    }
                    last = line;
                    count += 1;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if std::time::Instant::now() > deadline {
                        break;
                    }
                }
            }
        }
        for line in rx.try_iter() {
            last = line;
            count += 1;
        }
        assert!(saw_done, "子进程必须能写完 stderr 后正常退出");
        assert_eq!(count, 8192, "8192 行 stderr 必须全部收到，实际 {count}");
        assert!(first.starts_with("ERR-1-"), "首行: {first}");
        assert!(last.starts_with("ERR-8192-"), "末行: {last}");

        match old_script {
            Some(v) => std::env::set_var("HARNESS_SUPERVISE_SCRIPT", v),
            None => std::env::remove_var("HARNESS_SUPERVISE_SCRIPT"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
