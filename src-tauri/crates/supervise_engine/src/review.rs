//! 审查器：读会话 JSONL 做验收裁决。真实实现 spawn `codex exec`（从
//! supervise.ps1 移植，含 MCP 管道竞态重试），测试用 MockReviewer。
//! 审查可被取消：轮询等待子进程，取消时立即 kill——不然取消/窗口关闭
//! 要等整次 codex exec（分钟级）才生效，退出后还会留孤儿进程。

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub pass: bool,
    pub reason: String,
}

pub trait Reviewer: Send + Sync {
    /// 审查一轮。cancel 置位时应尽快返回 Err("已取消") 并终止子进程。
    /// 重试在实现内部完成（引擎层不叠加，防嵌套重试放大取消延迟）
    fn review(&self, transcript: &Path, round: i64, cancel: &AtomicBool) -> Result<Verdict, String>;
}

// ---------------- codex exec 审查（移植自 supervise.ps1 Invoke-CodexReview） ----------------

pub struct CodexReviewer {
    /// 审查模型。None = 不传 -m，用 codex 自己配置的默认模型——硬编码模型名
    /// 在中转服务/账号分组变更时会 404（真实事故：gpt-5.6-luna 不被支持）
    pub model: Option<String>,
    pub task: String,
    pub retries: u32,
    pub retry_wait_secs: u64,
}

impl CodexReviewer {
    pub fn new(model: Option<&str>, task: &str) -> Self {
        Self {
            model: model.filter(|m| !m.trim().is_empty()).map(String::from),
            task: task.to_string(),
            retries: 5,
            retry_wait_secs: 8,
        }
    }

    fn prompt(&self, transcript: &Path) -> String {
        // codex exec 有文件系统访问（bypass 模式），直接读会话文件，不依赖 MCP
        format!(
            "你是监督者。读取会话文件 {}，审查任务「{}」的完成情况：1) 任务完成度 \
             2) 方案合理性 3) 风险/遗漏。最后一行必须输出 [VERDICT] PASS 或 \
             [VERDICT] REVIEW + 一句 Claude 能直接执行的返工指令。",
            transcript.display(),
            self.task
        )
    }
}

/// 构造 codex exec 命令行。Windows 上 npm 安装的 codex 是 .cmd 垫片，裸名
/// spawn 找不到（CreateProcessW 不解析 PATHEXT，只会找 codex.exe）——必须走
/// cmd.exe /c 转发（与 terminal_host::launch 启动 CLI 同款方案）。
/// 真实事故：裸名 spawn 连续 5 次 os error 2，整场监督以"审查失败"中止。
/// model 为 None 时不传 -m：跟随 codex 配置的默认模型（硬编码模型名遇
/// 中转分组不支持时报 404）。
pub fn build_codex_command(model: Option<&str>, prompt: &str) -> (String, Vec<String>) {
    let mut base = vec![
        "exec".to_string(),
        "--skip-git-repo-check".to_string(),
        "--dangerously-bypass-approvals-and-sandbox".to_string(),
    ];
    if let Some(model) = model.filter(|m| !m.trim().is_empty()) {
        base.push("-m".to_string());
        base.push(model.to_string());
    }
    base.push(prompt.to_string());
    if cfg!(windows) {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let mut args = vec!["/d".into(), "/s".into(), "/c".into(), "codex".into()];
        args.extend(base);
        (comspec, args)
    } else {
        ("codex".to_string(), base)
    }
}

/// 保留字符串尾部 n 个字符（诊断信息用）
fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

impl Reviewer for CodexReviewer {
    fn review(&self, transcript: &Path, _round: i64, cancel: &AtomicBool) -> Result<Verdict, String> {
        let prompt = self.prompt(transcript);
        let mut last_err = String::new();
        for attempt in 1..=self.retries {
            if cancel.load(Ordering::Relaxed) {
                return Err("已取消".into());
            }
            // stdin 置 null：codex 在非 TTY 环境会读 stdin 附加输入而挂起；
            // CREATE_NO_WINDOW：发布版 GUI 子系统不弹控制台；
            // 命令走 build_codex_command（Windows 需 cmd.exe 垫片解析 codex.cmd）
            let (program, args) = build_codex_command(self.model.as_deref(), &prompt);
            let mut command = Command::new(&program);
            path_util::no_console_window(&mut command);
            let mut child = command
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                // stderr 后台消费（防写满挂死）并留尾部：审查失败时能看到
                // codex 到底报了什么，而不是只有一个退出码
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("spawn codex 失败（{program}，请确认 codex CLI 已安装）: {e}"))?;

            let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            {
                let buf = stderr_buf.clone();
                let pipe = child.stderr.take();
                std::thread::spawn(move || {
                    if let Some(mut pipe) = pipe {
                        use std::io::Read;
                        let mut sink = String::new();
                        let mut chunk = [0u8; 1024];
                        while let Ok(n) = pipe.read(&mut chunk) {
                            if n == 0 {
                                break;
                            }
                            sink.push_str(&String::from_utf8_lossy(&chunk[..n]));
                            // 只留尾部 4KB
                            if sink.len() > 8192 {
                                sink = tail(&sink, 4096);
                            }
                        }
                        if let Ok(mut b) = buf.lock() {
                            *b = sink;
                        }
                    }
                });
            }

            // 轮询等待而非 output()：取消能在 200ms 内 kill 子进程退出
            loop {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("已取消".into());
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let mut out = String::new();
                        if let Some(mut so) = child.stdout.take() {
                            use std::io::Read;
                            let _ = so.read_to_string(&mut out);
                        }
                        if let Some(v) = parse_verdict(&out) {
                            return Ok(v);
                        }
                        let stderr_tail = stderr_buf
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .clone();
                        last_err = match status.code() {
                            Some(0) => format!(
                                "第 {attempt} 次未解析到 VERDICT。codex 输出尾部: {}",
                                tail(&out, 200)
                            ),
                            Some(code) => format!(
                                "codex 退出码 {code}（第 {attempt} 次）。stderr 尾部: {}",
                                tail(&stderr_tail, 200)
                            ),
                            None => format!("codex 被信号终止（第 {attempt} 次）"),
                        };
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(200)),
                    Err(e) => {
                        last_err = format!("等待 codex 失败: {e}");
                        break;
                    }
                }
            }
            if attempt < self.retries {
                std::thread::sleep(Duration::from_secs(self.retry_wait_secs));
            }
        }
        Err(format!("codex 审查连续 {} 次失败：{last_err}", self.retries))
    }
}

/// 清理裁决理由：剥掉 codex 输出携带的 Markdown 列表前缀（"+ / - / *"）——
/// 真实输出出现过 "需返工：+ 请在 …"（列表符直接拼进理由）
fn clean_reason(raw: &str) -> String {
    let r = raw.trim();
    let r = r
        .strip_prefix("+ ")
        .or_else(|| r.strip_prefix("- "))
        .or_else(|| r.strip_prefix("* "))
        .unwrap_or(r);
    r.trim().to_string()
}

/// 从 codex 输出解析 [VERDICT]。取最后一个匹配且跳过提示词回显行
/// （codex 非 TTY 会回显 prompt，其中含指令模板本身）。
pub fn parse_verdict(raw: &str) -> Option<Verdict> {
    let mut found: Option<Verdict> = None;
    for line in raw.split('\n') {
        let Some(m) = line.find("[VERDICT]") else { continue };
        // 提示词回显特征：模板里写明了输出要求，跳过
        if line.contains("最后一行必须输出") {
            continue;
        }
        let rest = &line[m + "[VERDICT]".len()..];
        let rest = rest.trim_start();
        if let Some(reason) = rest.strip_prefix("PASS") {
            found = Some(Verdict {
                pass: true,
                reason: clean_reason(reason),
            });
        } else if let Some(reason) = rest.strip_prefix("REVIEW") {
            found = Some(Verdict {
                pass: false,
                reason: clean_reason(reason),
            });
        }
    }
    found
}

// ---------------- Mock ----------------

/// 脚本化审查器：按序返回预置结果（耗尽后重复最后一个）
pub struct MockReviewer {
    scripted: std::sync::Mutex<Vec<Result<Verdict, String>>>,
    always: Option<Result<Verdict, String>>,
}

impl MockReviewer {
    pub fn always(result: Result<Verdict, String>) -> Self {
        Self {
            scripted: std::sync::Mutex::new(vec![]),
            always: Some(result),
        }
    }

    pub fn scripted(results: Vec<Result<Verdict, String>>) -> Self {
        Self {
            scripted: std::sync::Mutex::new(results),
            always: None,
        }
    }
}

impl Reviewer for MockReviewer {
    fn review(&self, _transcript: &Path, _round: i64, _cancel: &AtomicBool) -> Result<Verdict, String> {
        if let Some(a) = &self.always {
            return a.clone();
        }
        let mut q = self.scripted.lock().unwrap();
        if q.len() > 1 {
            q.remove(0)
        } else {
            q.first().cloned().unwrap_or(Ok(Verdict {
                pass: true,
                reason: "mock 默认通过".into(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verdict_takes_last_and_skips_echo() {
        let raw = "审查中…\n最后一行必须输出 [VERDICT] PASS 或 [VERDICT] REVIEW + 指令\n\
                   分析：缺测试\n[VERDICT] REVIEW 请补充边界测试\nsome tail\n";
        let v = parse_verdict(raw).expect("应解析");
        assert!(!v.pass);
        assert_eq!(v.reason, "请补充边界测试");

        let pass = parse_verdict("开头\n[VERDICT] PASS 一次通过\n[VERDICT] PASS\n");
        assert_eq!(pass.map(|v| (v.pass, v.reason)), Some((true, "".to_string())));
    }

    #[test]
    fn parse_verdict_none_when_absent() {
        assert!(parse_verdict("没有任何结论的输出").is_none());
    }

    /// codex 输出会把 Markdown 列表符拼进理由（真实输出："REVIEW + 请在 …"）
    #[test]
    fn parse_verdict_strips_list_marker_prefix() {
        let v = parse_verdict("[VERDICT] REVIEW + 请补充边界测试\n").expect("应解析");
        assert_eq!(v.reason, "请补充边界测试");
        let v2 = parse_verdict("[VERDICT] PASS - 一次通过\n").expect("应解析");
        assert_eq!(v2.reason, "一次通过");
    }

    /// Windows 必须走 cmd.exe 垫片：裸名 spawn 找不到 npm 的 codex.cmd
    /// （CreateProcessW 不解析 PATHEXT），真实事故为连续 5 次 os error 2
    #[cfg(windows)]
    #[test]
    fn codex_command_goes_through_cmd_shim_on_windows() {
        let (program, args) = build_codex_command(Some("gpt-x"), "审查提示词");
        assert!(program.to_lowercase().ends_with("cmd.exe"), "program: {program}");
        assert_eq!(
            &args[..4],
            &[
                "/d".to_string(),
                "/s".to_string(),
                "/c".to_string(),
                "codex".to_string()
            ]
        );
        assert_eq!(args[4], "exec");
        assert_eq!(args[args.len() - 3], "-m");
        assert_eq!(args[args.len() - 2], "gpt-x");
        assert_eq!(args.last().unwrap(), "审查提示词", "提示词必须是最后一个参数");
    }

    /// 默认不传 -m：跟随 codex 配置的默认模型（硬编码模型名 404 事故）
    #[test]
    fn codex_command_omits_model_flag_when_none() {
        let (_program, args) = build_codex_command(None, "提示词");
        assert!(!args.contains(&"-m".to_string()), "None 时不传 -m: {args:?}");
        assert_eq!(args.last().unwrap(), "提示词");
    }

    #[test]
    fn tail_keeps_last_chars() {
        assert_eq!(tail("abcdefghij", 3), "hij");
        assert_eq!(tail("ab", 5), "ab");
    }
}
