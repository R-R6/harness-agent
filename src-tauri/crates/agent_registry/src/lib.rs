//! agent_registry —— 多 Agent 监督的 CLI Agent 注册表
//!
//! 每个受支持的 CLI 终端 Agent 一条 [`AgentProfile`]：角色能力（工人/审查者）、
//! 交互与 headless 命令模板、会话根目录、停轮与信任策略。
//! 新增一家 CLI = catalog 加一条 profile（+可选会话适配器），不改引擎与 UI 框架。
//!
//! 设计依据（docs/设计规格/多Agent监督体系开发设计.md）：
//! - vibe-kanban：单一执行器抽象 + capability 位 + 默认 profile 注册表
//! - goose Lead/Worker：监督方/被监督方两个槽位独立选择
//! - 审查只是 headless spawn 的变体（codex exec / claude -p / gemini -p / grok -p）

/// 停轮检测策略：一轮"干活结束"如何被引擎感知
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundEnd {
    /// Claude Code：settings.json Stop hook 写 marker（现状）
    Hook,
    /// 无钩子的 CLI：靠会话静默兜底（引擎既有路径）
    Silence,
}

/// 首次进入目录的信任策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// 写 ~/.claude.json 的 hasTrustDialogAccepted（含斜杠/大小写变体）
    ClaudeJson,
    /// 写 ~/.gemini/trustedFolders.json 的 "<path>": true
    GeminiTrustedFolders,
    /// 无信任对话框（或细节未证实），交给注入前的人工等待兜底
    None,
}

/// 一条 CLI Agent 的完整描述。字段均为静态声明，注册表编译期固定。
#[derive(Debug, Clone, Copy)]
pub struct AgentProfile {
    /// 稳定 id（前后端契约，如 "claude"）
    pub id: &'static str,
    /// 显示名
    pub name: &'static str,
    /// 交互 CLI 名（Windows 上经 cmd shim 解析 .cmd）
    pub command: &'static str,
    /// 可作被监督方：能以交互 PTY 模式被注入任务
    pub can_work: bool,
    /// 可作监督方：有 headless 一次性模式可产出审查结论
    pub can_review: bool,
    /// headless 审查命令模板：prompt 前的固定参数（prompt 本身总是最后一个参数）
    pub review_args: &'static [&'static str],
    /// 会话根目录（相对 HOME 的固定部分，供状态探测与后续会话适配）
    pub session_roots: &'static [&'static str],
    /// 停轮检测策略
    pub round_end: RoundEnd,
    /// 首次进入目录的信任策略
    pub trust: Trust,
    /// 最小 headless 查询参数（端点预检用）
    pub preflight_args: &'static [&'static str],
}

/// 内置注册表。顺序即 UI 展示顺序；claude/codex 保持既有默认角色。
pub const CATALOG: &[AgentProfile] = &[
    AgentProfile {
        id: "claude",
        name: "Claude Code",
        command: "claude",
        can_work: true,
        can_review: true,
        // claude -p <prompt>：无头一次性执行
        review_args: &["-p"],
        session_roots: &[".claude/projects"],
        round_end: RoundEnd::Hook,
        trust: Trust::ClaudeJson,
        preflight_args: &["-p", "ping", "--max-turns", "1"],
    },
    AgentProfile {
        id: "codex",
        name: "Codex CLI",
        command: "codex",
        can_work: true,
        can_review: true,
        // codex exec <prompt>：无头一次性执行
        review_args: &["exec"],
        session_roots: &[".codex/sessions"],
        round_end: RoundEnd::Silence,
        trust: Trust::None,
        preflight_args: &["exec", "reply with: ok"],
    },
    AgentProfile {
        id: "gemini",
        name: "Gemini CLI",
        command: "gemini",
        can_work: true,
        can_review: true,
        // gemini -p <prompt>：非 TTY 自动 headless
        review_args: &["-p"],
        session_roots: &[".gemini/tmp"],
        round_end: RoundEnd::Silence,
        trust: Trust::GeminiTrustedFolders,
        preflight_args: &["-p", "reply with: ok"],
    },
    AgentProfile {
        id: "grok",
        name: "Grok Build",
        command: "grok",
        can_work: true,
        can_review: true,
        // grok -p <prompt>（官方 xai-org/grok-build）
        review_args: &["-p"],
        session_roots: &[".grok/sessions"],
        round_end: RoundEnd::Silence,
        trust: Trust::None,
        preflight_args: &["-p", "reply with: ok"],
    },
    AgentProfile {
        id: "dsh",
        name: "DeepSeek DSH",
        command: "dsh",
        can_work: true,
        can_review: true,
        // dsh --profile headless <job>
        review_args: &["--profile", "headless"],
        session_roots: &[".dsh"],
        round_end: RoundEnd::Silence,
        trust: Trust::None,
        preflight_args: &["--profile", "headless", "reply with: ok"],
    },
];

/// 按 id 取 profile
pub fn get(id: &str) -> Option<&'static AgentProfile> {
    CATALOG.iter().find(|p| p.id == id)
}

/// 本机探测结果（状态卡 / 角色选择器的数据源）
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentStatus {
    pub id: &'static str,
    pub name: &'static str,
    pub can_work: bool,
    pub can_review: bool,
    /// CLI 在 PATH 中可解析
    pub installed: bool,
    /// 任一会话根目录存在（提示有历史会话，非必须）
    pub sessions_present: bool,
}

impl AgentStatus {
    pub fn of(profile: &AgentProfile, home: &std::path::Path) -> Self {
        let installed = terminal_host::agent_command_exists(profile.command);
        let sessions_present = profile
            .session_roots
            .iter()
            .any(|root| home.join(root).is_dir());
        Self {
            id: profile.id,
            name: profile.name,
            can_work: profile.can_work,
            can_review: profile.can_review,
            installed,
            sessions_present,
        }
    }
}

/// 全量状态（注册表顺序）
pub fn status_all(home: &std::path::Path) -> Vec<AgentStatus> {
    CATALOG.iter().map(|p| AgentStatus::of(p, home)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_unique_ids_and_defaults() {
        let mut ids: Vec<_> = CATALOG.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len(), "id 必须唯一");

        let claude = get("claude").expect("claude 必须在注册表");
        assert!(claude.can_work && claude.can_review);
        assert_eq!(claude.round_end, RoundEnd::Hook);
        assert_eq!(claude.trust, Trust::ClaudeJson);

        let codex = get("codex").expect("codex 必须在注册表");
        assert_eq!(codex.round_end, RoundEnd::Silence);
        assert_eq!(codex.review_args, &["exec"]);

        let dsh = get("dsh").expect("dsh 必须在注册表");
        assert_eq!(dsh.review_args, &["--profile", "headless"]);
    }

    #[test]
    fn every_review_capable_agent_has_preflight_and_prompt_args() {
        for p in CATALOG {
            if p.can_review {
                assert!(
                    !p.review_args.is_empty(),
                    "{} 的 review_args 不能为空",
                    p.id
                );
                assert!(
                    !p.preflight_args.is_empty(),
                    "{} 的 preflight_args 不能为空",
                    p.id
                );
            }
            assert!(!p.session_roots.is_empty(), "{} 需声明会话根目录", p.id);
        }
    }

    #[test]
    fn status_all_reports_every_profile() {
        let home = std::env::temp_dir();
        let statuses = status_all(&home);
        assert_eq!(statuses.len(), CATALOG.len());
        for (s, p) in statuses.iter().zip(CATALOG) {
            assert_eq!(s.id, p.id);
            assert_eq!(s.name, p.name);
            // temp 下没有会话目录
            assert!(!s.sessions_present);
        }
    }

    #[test]
    fn status_detects_session_root_fixture() {
        let dir = std::env::temp_dir().join(format!("ha-agents-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".gemini/tmp/hash/chats")).unwrap();
        let statuses = status_all(&dir);
        let gemini = statuses.iter().find(|s| s.id == "gemini").unwrap();
        assert!(gemini.sessions_present);
        let claude = statuses.iter().find(|s| s.id == "claude").unwrap();
        assert!(!claude.sessions_present);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn get_unknown_id_is_none() {
        assert!(get("nope").is_none());
    }
}
