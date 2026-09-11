# 开发设计：多 Agent 监督体系

> 对应 PRD：docs/设计规格/多Agent监督体系产品需求.md
> 分支：feature/multi-agent-supervision · 分波次落地，每波次 = 可独立提交推送的功能点

## 0. 现状与改动面

- `terminal_host::terminal_command(agent, args)`：已按 agent 字符串解析命令（claude 原生 exe 优先，其余走 cmd shim）。**扩展点：注册表化**。
- `supervise_engine::CodexReviewer`：审查= spawn `codex exec`（经 cmd shim），读 stdout 判定。**扩展点：命令模板化**。
- `run_supervise_terminal`：绑定一个 claude PTY pane（`PaneIo` trait 已是工人无关抽象），Stop hook 为 claude 专属。**扩展点：hook 按 profile 门控**。
- 会话浏览：`session_proxy` server.js 列 claude+codex。**扩展点：来源适配器**。

## 1. Wave 1 —— Agent 注册表（crate: agent_registry）

新 crate `src-tauri/crates/agent_registry`（不依赖 tauri，可测试）：

```rust
pub struct AgentProfile {
    pub id: &'static str,           // "claude"
    pub name: &'static str,         // "Claude Code"
    pub command: &'static str,      // 交互 CLI 名（cmd shim 解析）
    pub can_work: bool,             // PTY 交互工人
    pub can_review: bool,           // 有 headless 模式
    /// headless 审查模板：{prompt} 占位
    pub review_args: &'static [&'static str],
    pub review_prompt_flag: &'static str, // claude/codex/grok: 位置或 flag
    pub session_roots: &'static [&'static str], // 相对 HOME
    pub round_end: RoundEnd,        // Hook | Silence
    pub trust: Trust,               // ClaudeJson | GeminiTrustedFolders | None
    pub preflight_args: &'static [&'static str], // 最小 headless 查询
}
pub fn catalog() -> &'static [AgentProfile];
pub fn get(id: &str) -> Option<&'static AgentProfile>;
```

初版 5 条：claude、codex、gemini、grok、dsh（dsh: can_work=true, can_review=true, session_roots=[".dsh"]）。
`installed(home, path_lookup)`：命令解析 + 会话根目录存在性 → `AgentStatus { profile, installed, version? }`。
测试：profile 契约（字段完备、id 唯一、claude/codex 与现状一致）、状态探测（fixture HOME）。

## 2. Wave 2 —— 终端工作台多 Agent

- `terminal_command` 接注册表：非 claude/codex 的 agent 走 `cmd /c <command>` shim（与 codex 同路），claude 保留原生 exe 解析。
- `start_terminal` 的预信任按 `trust` 策略：claude（现状）+ gemini（写 `~/.gemini/trustedFolders.json` 的 `"<path>": true`）；失败发 terminal-notice（既有通道）。
- 前端：`claudeTabs` 泛化为 `workerTabs`（标签名 = profile.name），工作台 intro 区渲染 Agent 启动条（catalog 状态卡片，点击=开 pane）。
- 测试：terminal_command 注册表路由、trustedFolders 写入、前端 tab 泛化回归。

## 3. Wave 3 —— 审查方通用化

- `supervise_engine`：`CodexReviewer` → `CliReviewer { profile, prompt }`，按 `review_args` 模板拼 headless 命令（codex exec 保留现状；claude/gemini/grok：`-p <prompt>`；dsh：`--profile headless <prompt>`）。verdict 解析复用 review::parse_verdict（markdown「判定：」协议，写入审查提示词，与 Agent 无关）。
- `SuperviseRequest` 增 `reviewer_agent: Option<String>`（缺省 codex，兼容旧前端）。
- 预检泛化：`preflight_agent(profile_id)` 用 `preflight_args`。
- 测试：命令模板（不真 spawn）、缺省 codex 回归、verdict 协议。

## 4. Wave 4 —— 任务表单双选择器 + 引擎接线

- `run_supervise_terminal` 增 `worker_agent`：pane 检索按 `agent == worker_agent`（claude 缺省回归）；Stop hook 仅 `worker=="claude" && !mock` 安装；非 claude 工人依赖静默停轮（引擎已有）。
- 前端表单：被监督方下拉（can_work 且该目录有 pane 或将新建）、监督方下拉（can_review），默认 claude/codex，记忆上次选择。
- 测试：worker_agent 缺省回归、hook 门控、表单选择器渲染与提交契约。

## 5. Wave 5 —— 跨 Agent 会话管理

- server.js（session_proxy）来源适配器化：claude/codex（现状）+ gemini（`~/.gemini/tmp/*/chats/sessions-*.jsonl` 首行元数据：sessionId/cwd/mtime）+ grok（`~/.grok/sessions/` 枚举）。dsh 仅状态。
- 统一 schema：`{agent, id, title, cwd, mtime, transcript_path}`；未适配格式 → `unsupported: true`。
- 会话浏览 UI：来源筛选下拉改为动态（有数据的 Agent）；无适配器的 Agent 显示"格式适配中"。
- 测试：fixture 会话文件解析（gemini JSONL/grok 目录约定）、空/损坏容错、UI 筛选。

## 6. 风险与对策

| 风险 | 对策 |
|---|---|
| gemini projectHash 与 cwd 映射不明 | 元数据行内含 cwd 则用；否则全量列出 + 时间排序，不做目录过滤 |
| grok/dsh 文档描述与实机格式漂移 | 适配器容错（解析失败单条跳过）+ fixture 测试；dsh 明确标"未适配" |
| 非 claude 工人停轮依赖静默（拖时间） | v1 接受；wave 6+ 接 gemini AfterAgent / grok turn-end hook |
| 同型号自审偏软 | UI 提示，不禁止 |

## 7. 交付节奏

每个 Wave：实现 → cargo/vitest 全绿 → （涉及 UI）CDP 真机验证 → 独立 commit + push。
收尾：code-review 双轴 → 修复 → 全量回归 → 推送 → 汇报。
