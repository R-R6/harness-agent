# 产品需求：多 Agent 监督体系（Multi-Agent Supervision）

> 状态：已定稿 v1 · 2026-09-12
> 来源：调研 vibe-kanban / claude-squad / opencode / goose 的编排模式 + Gemini CLI / Grok Build / DeepSeek DSH 官方文档
> 关联分支：feature/multi-agent-supervision

## 1. 背景与问题

当前监督闭环把角色写死：**被监督方（工人）永远是 Claude Code，监督方（审查者）永远是 Codex**。
用户希望接入更多 CLI 终端 Agent（Gemini CLI、Grok Build、DeepSeek DSH……），自由选择**谁干活、谁审查**，并且以后出新 CLI 能低成本补充。

## 2. 调研结论（设计依据）

| CLI | 命令 | headless | 会话落盘 | turn-end 钩子 | 信任机制 |
|---|---|---|---|---|---|
| Claude Code | `claude` | `claude -p` | `~/.claude/projects/<slug>/*.jsonl` | Stop hook（已用） | `~/.claude.json` hasTrustDialogAccepted |
| Codex | `codex` | `codex exec` | `~/.codex/sessions/**` rollout | notify | 无信任框 |
| Gemini CLI | `gemini` | `gemini -p`（--output-format json） | `~/.gemini/tmp/<hash>/chats/sessions-*.jsonl` | hooks（AfterAgent 最接近） | `~/.gemini/trustedFolders.json` |
| Grok Build | `grok` | `grok -p` | `~/.grok/sessions/` | hooks（turn-end） | 存在（细节待验） |
| DSH (DeepSeek Harness) | `dsh` | `dsh --profile headless "job"` | `~/.dsh/`（事件日志，可 resume/fork） | 插件 turn-end 事件 | 未证实 |

关键借鉴：
1. **vibe-kanban**：单一执行器 trait + capability 位 + 每家一个适配器 + 默认 profile 注册表；"审查"只是 headless spawn 的变体，不另立抽象。新增一家改 3~5 处。
2. **goose Lead/Worker**：领导/工人两个槽位各自可配 provider，天然支持"谁监督谁"。
3. **claude-squad**：profile 就是一条 shell 命令，最简可用。

## 3. 产品需求

### P1 Agent 注册表（可扩展的根基）
- 系统内置 Agent 注册表：claude、codex、gemini、grok、dsh。每条 profile 声明：显示名、交互命令、能否当工人（can_work）、能否当审查者（can_review）、headless 审查命令模板、会话根目录、停轮策略、信任策略。
- **新增一家 CLI = 注册表加一条 profile + （可选）一个会话适配器**，不改引擎、不改 UI 框架。
- 工作台展示每个 Agent 的本机状态：未安装 / 已安装（版本）/ 探测中。

### P2 谁监督谁（角色选择）
- 任务创建表单提供两个选择器：**被监督方**（can_work 且当前目录有空闲 pane 的 Agent）、**监督方**（can_review 的 Agent）。
- 默认值维持现状：工人=claude、审查者=codex；选择记忆在上次使用。
- 允许同型号组合（如 claude 监督 claude），UI 提示"同型号自审可能偏软"。
- 审查者是 headless 一次性进程：不需要 PTY pane，跑完拿退出码+输出即可。

### P3 终端工作台多 Agent
- Claude 列的 tab 体系泛化为「工人列」：任意 can_work Agent 都能开 pane（标签显示 Agent 名）。
- Codex 右列保留为快捷位；更多 Agent 从工作台的 Agent 启动条进入。
- 信任/预信任按 Agent 策略处理：Claude 写 hasTrustDialogAccepted；Gemini 写 trustedFolders.json；其他跳过（弹框由 wait_for_input_ready 的人工等待兜底）。

### P4 跨 Agent 会话管理
- 会话浏览按 **Agent 来源** 分组筛选（现有 Claude/Codex 下拉自然扩展）。
- 会话列表统一 schema：{agent, 会话 id, 标题, cwd, 时间}；transcript 阅读器 v1 支持 claude/codex，其他 Agent 展示列表 + 原始文件定位（阅读器后续补）。
- 会话格式未适配的 Agent：列表位置显示"已检测到 CLI，会话格式适配中"，不出假数据。

### P5 非功能
- 工人端点预检按 profile 模板执行（现有 preflight 机制泛化）。
- 引擎停轮检测按 profile 策略：claude=Stop hook（现状不变）；其他=会话静默兜底（引擎已有 silence 路径）；gemini/grok 的 turn-end 钩子列入后续增强。
- 所有新能力离线可测（fixture + mock），CI 跑全量。

## 4. 明确不做（v1 边界）
- 不做 Agent 间自由对话编排（只保留工人/审查者单向闭环）。
- 不做 gemini/grok/dsh 的 Stop hook 安装（静默兜底已可用）。
- 不做各 Agent 模型级选择（用各家默认模型）。
- 不做 DSH 会话 transcript 解析（格式未稳定，只做状态探测）。

## 5. 验收标准
1. 注册表驱动：新 Agent 加入后，工作台能启动它、任务表单能选它当工人/审查者、会话浏览出现它的来源筛选项（有适配器时）。
2. claude+codex 组合全流程回归不变（含真机两轮闭环）。
3. gemini 当审查者、claude 当工人（或反之）在本机可跑通一轮（若本机已安装）。
4. cargo + vitest 全绿；每个功能点独立提交推送。
