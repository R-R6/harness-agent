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

// ---------------- CLI headless 审查（多 Agent：codex exec / claude -p / gemini -p / ...） ----------------

pub struct CodexReviewer {
    /// 审查 Agent（agent_registry id）。名字保留 CodexReviewer 以减少波次改动，
    /// 实际支持任意 can_review 的注册表 Agent（for_agent 构造）。
    pub agent: &'static str,
    /// 审查模型。None = 不传 -m，用 CLI 自己配置的默认模型——硬编码模型名
    /// 在中转服务/账号分组变更时会 404（真实事故：gpt-5.6-luna 不被支持）
    pub model: Option<String>,
    pub task: String,
    pub retries: u32,
    pub retry_wait_secs: u64,
}

impl std::fmt::Debug for CodexReviewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexReviewer")
            .field("agent", &self.agent)
            .field("model", &self.model)
            .finish()
    }
}

impl CodexReviewer {
    pub fn new(model: Option<&str>, task: &str) -> Self {
        Self::for_agent("codex", model, task).expect("codex 在注册表且可审查")
    }

    /// 按注册表 Agent 构造审查器。codex 保留专属 bypass 旗标（历史行为），
    /// 其余走 profile.review_args 通用模板。
    pub fn for_agent(agent_id: &str, model: Option<&str>, task: &str) -> Result<Self, String> {
        let profile = agent_registry::get(agent_id)
            .copied()
            .ok_or_else(|| format!("未注册的 Agent: {agent_id}"))?;
        if !profile.can_review {
            return Err(format!("{} 不支持作为监督方（无 headless 模式）", profile.name));
        }
        Ok(Self {
            agent: profile.id,
            model: model.filter(|m| !m.trim().is_empty()).map(String::from),
            task: task.to_string(),
            retries: 5,
            retry_wait_secs: 8,
        })
    }

    fn prompt(&self, transcript: &Path) -> String {
        // codex exec 有文件系统访问（bypass 模式），直接读会话文件，不依赖 MCP；
        // 其余 CLI 的 headless 同样具备工作区读权限。措辞工人无关。
        format!(
            "你是监督者。读取会话文件 {}，审查任务「{}」的完成情况：1) 任务完成度 \
             2) 方案合理性 3) 风险/遗漏。最后一行必须输出 [VERDICT] PASS 或 \
             [VERDICT] REVIEW + 一句工人 CLI 能直接执行的返工指令。",
            transcript.display(),
            self.task
        )
    }
}

/// 构造审查命令行。Windows 上 npm 安装的 CLI 是 .cmd 垫片，裸名
/// spawn 找不到（CreateProcessW 不解析 PATHEXT，只会找 .exe）——必须走
/// cmd.exe /c 转发（与 terminal_host::launch 启动 CLI 同款方案）。
/// 真实事故：裸名 spawn 连续 5 次 os error 2，整场监督以"审查失败"中止。
/// model 为 None 时不传 -m：跟随 CLI 配置的默认模型（硬编码模型名遇
/// 中转分组不支持时报 404）。
///
/// codex 保留专属旗标（exec + bypass 审批沙箱，移植自 supervise.ps1）；
/// 其余注册表 Agent 用 profile.review_args 模板（claude/gemini/grok: `-p`，
/// dsh: `--profile headless`），prompt 恒为最后一个参数。
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
    wrap_cmd_shim("codex", base)
}

/// 通用模板版：按注册表 id 构造 headless 审查命令
pub fn build_agent_review_command(agent_id: &str, model: Option<&str>, prompt: &str) -> Result<(String, Vec<String>), String> {
    let _ = model; // codex 路径透传 model；通用路径 v1 不透传（见下）
    if agent_id == "codex" {
        return Ok(build_codex_command(model, prompt));
    }
    let profile = agent_registry::get(agent_id)
        .copied()
        .ok_or_else(|| format!("未注册的 Agent: {agent_id}"))?;
    let mut base: Vec<String> = profile.review_args.iter().map(|s| s.to_string()).collect();
    // 注意：非 codex 路径不透传 model——各家 CLI 的模型 flag 形态不一（--model / 插件层配置），
    // 盲推 -m 未经验证；v1 一律跟随该 CLI 自己配置的默认模型
    base.push(prompt.to_string());
    Ok(wrap_cmd_shim(profile.command, base))
}

/// cmd.exe 垫片包装（仅 Windows；其他平台直接裸命令）
fn wrap_cmd_shim(command: &str, args: Vec<String>) -> (String, Vec<String>) {
    if cfg!(windows) {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let mut wrapped = vec!["/d".into(), "/s".into(), "/c".into(), command.to_string()];
        wrapped.extend(args);
        (comspec, wrapped)
    } else {
        (command.to_string(), args)
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
            // stdin 置 null：CLI 在非 TTY 环境会读 stdin 附加输入而挂起；
            // CREATE_NO_WINDOW：发布版 GUI 子系统不弹控制台；
            // 命令走 build_agent_review_command（Windows 需 cmd.exe 垫片解析 .cmd）
            let (program, args) =
                build_agent_review_command(self.agent, self.model.as_deref(), &prompt)?;
            let mut command = Command::new(&program);
            path_util::no_console_window(&mut command);
            let mut child = command
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                // stderr 后台消费（防写满挂死）并留尾部：审查失败时能看到
                // CLI 到底报了什么，而不是只有一个退出码
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("spawn {} 失败（{program}，请确认 CLI 已安装）: {e}", self.agent))?;

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
                                "第 {attempt} 次未解析到 VERDICT。{} 输出尾部: {}", self.agent,
                                tail(&out, 200)
                            ),
                            Some(code) => format!(
                                "{} 退出码 {code}（第 {attempt} 次）。stderr 尾部: {}", self.agent,
                                tail(&stderr_tail, 200)
                            ),
                            None => format!("{} 被信号终止（第 {attempt} 次）", self.agent),
                        };
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(200)),
                    Err(e) => {
                        last_err = format!("等待 {} 失败: {e}", self.agent);
                        break;
                    }
                }
            }
            if attempt < self.retries {
                std::thread::sleep(Duration::from_secs(self.retry_wait_secs));
            }
        }
        Err(format!("{} 审查连续 {} 次失败：{last_err}", self.agent, self.retries))
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

    /// 多 Agent：通用模板构造（claude/gemini 用 -p，dsh 用 --profile headless）
    #[test]
    fn agent_review_command_follows_registry_template() {
        for (agent, flag) in [("claude", "-p"), ("gemini", "-p"), ("grok", "-p")] {
            let (program, args) = build_agent_review_command(agent, None, "审查").unwrap();
            assert!(program.to_lowercase().ends_with("cmd.exe"), "{program}");
            assert_eq!(args[3], agent);
            assert!(args.contains(&flag.to_string()), "{args:?}");
            assert_eq!(args.last().unwrap(), "审查");
        }
        let (_p, dsh_args) = build_agent_review_command("dsh", None, "审查").unwrap();
        assert!(dsh_args.contains(&"--profile".to_string()));
        assert!(dsh_args.contains(&"headless".to_string()));
        assert_eq!(dsh_args.last().unwrap(), "审查");
    }

    /// 多 Agent：通用路径 v1 不透传模型（各家 CLI 模型 flag 形态不一，盲推 -m
    /// 未经验证；跟随该 CLI 自己配置的默认模型——-m 404 事故教训）
    #[test]
    fn agent_review_command_ignores_model_flag() {
        let (_p, args) = build_agent_review_command("claude", Some("gemini-2x"), "审查").unwrap();
        assert!(!args.contains(&"-m".to_string()), "{args:?}");
        assert_eq!(args.last().unwrap(), "审查");
    }

    /// 多 Agent：for_agent 校验注册表与能力
    #[test]
    fn for_agent_validates_registry_and_capability() {
        assert!(CodexReviewer::for_agent("claude", None, "任务").is_ok());
        assert!(CodexReviewer::for_agent("gemini", None, "任务").is_ok());
        let err = CodexReviewer::for_agent("nope", None, "任务").unwrap_err();
        assert!(err.contains("未注册"), "{err}");
        // 注册表里没有 can_review=false 的条目，构造不可达——直接断言 err 文案路径
        assert!(CodexReviewer::new(None, "任务").agent == "codex", "默认仍是 codex");
    }
}
