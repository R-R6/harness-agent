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
    pub model: String,
    pub task: String,
    pub retries: u32,
    pub retry_wait_secs: u64,
}

impl CodexReviewer {
    pub fn new(model: &str, task: &str) -> Self {
        Self {
            model: model.to_string(),
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

impl Reviewer for CodexReviewer {
    fn review(&self, transcript: &Path, _round: i64, cancel: &AtomicBool) -> Result<Verdict, String> {
        let prompt = self.prompt(transcript);
        let mut last_err = String::new();
        for attempt in 1..=self.retries {
            if cancel.load(Ordering::Relaxed) {
                return Err("已取消".into());
            }
            // stdin 置 null：codex 在非 TTY 环境会读 stdin 附加输入而挂起；
            // stderr 置 null：无人消费的管道写满会挂死子进程；
            // CREATE_NO_WINDOW：发布版 GUI 子系统不弹控制台
            let mut command = Command::new("codex");
            path_util::no_console_window(&mut command);
            let mut child = command
                .args([
                    "exec",
                    "--skip-git-repo-check",
                    "--dangerously-bypass-approvals-and-sandbox",
                ])
                .args(["-m", &self.model])
                .arg(&prompt)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("spawn codex 失败（请确认 codex CLI 已安装）: {e}"))?;

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
                        last_err = match status.code() {
                            Some(0) => format!("第 {attempt} 次未解析到 VERDICT"),
                            Some(code) => format!("codex 退出码 {code}（第 {attempt} 次）"),
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
                reason: reason.trim().to_string(),
            });
        } else if let Some(reason) = rest.strip_prefix("REVIEW") {
            found = Some(Verdict {
                pass: false,
                reason: reason.trim().to_string(),
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
}
