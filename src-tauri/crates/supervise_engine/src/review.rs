//! 审查器：读会话 JSONL 做验收裁决。真实实现 spawn `codex exec`（从
//! supervise.ps1 移植，含 MCP 管道竞态重试），测试用 MockReviewer。

use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub pass: bool,
    pub reason: String,
}

pub trait Reviewer: Send + Sync {
    fn review(&self, transcript: &Path, round: i64) -> Result<Verdict, String>;
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
    fn review(&self, transcript: &Path, _round: i64) -> Result<Verdict, String> {
        let prompt = self.prompt(transcript);
        let mut last_err = String::new();
        for attempt in 1..=self.retries {
            // stdin 置 null：codex 在非 TTY 环境会读 stdin 附加输入而挂起
            let output = Command::new("codex")
                .args([
                    "exec",
                    "--skip-git-repo-check",
                    "--dangerously-bypass-approvals-and-sandbox",
                ])
                .args(["-m", &self.model])
                .arg(&prompt)
                .stdin(Stdio::null())
                .output();
            match output {
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    if let Some(v) = parse_verdict(&text) {
                        return Ok(v);
                    }
                    last_err = format!("第 {attempt} 次未解析到 VERDICT");
                }
                Err(e) => {
                    last_err = format!("spawn codex 失败（第 {attempt} 次）: {e}");
                }
            }
            if attempt < self.retries {
                std::thread::sleep(std::time::Duration::from_secs(self.retry_wait_secs));
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
    fn review(&self, _transcript: &Path, _round: i64) -> Result<Verdict, String> {
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
