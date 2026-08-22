# 阶段 1.5 Spike 结论（2026-08-22）

为"监督引擎驱动可见终端"（阶段 2）钉死的三个不确定性。验证环境：本机
（Windows 11、Claude Code 2.1.238、ACP/OEM=936）。

## Spike A：交互会话的文件定位与增长监听 —— ✅ 成立

实测证据（`~/.claude/projects/F--project-workspace-side-Harness-agent/9c0d3315….jsonl`）：

- 672 行，时间戳从 `2026-08-21T11:13` 跨到 `2026-08-22T02:56`——交互会话
  期间 JSONL **实时增量追加**，不是会话结束才落盘
- 每行自带 `sessionId` + `timestamp`；类型分布含 assistant/user/queue-operation 等
- `queue-operation enqueue` 行真实存在：上一轮未结束时注入的文本会被 TUI
  排队顺序消费，**注入不会丢**
- 项目 slug 有损（`my_skils` → `my-skils`），无法从路径反解原始目录——
  已由 cwd 透出（49acc6e）解决

结论：文件增长监听用 std 轮询（500ms stat size/mtime）即可，无需 notify
依赖；但"定位是哪个文件"不必自己猜——见 Spike B 的 transcript_path。

## Spike B：轮次完成信号选型 —— ✅ Stop hook 为主、静默兜底

官方 hooks 参考（code.claude.com/docs/en/hooks）：

- **Stop hook 在主 agent 每轮结束（正常停止响应）时触发**
- stdin 收到 JSON，含 `session_id`、`transcript_path`、`stop_hook_active`——
  `transcript_path` 直接告诉我们"这个会话是哪个文件"，Spike A 的定位问题
  被它一并消解
- hook 输出 `{"decision":"block","reason":"…"}` 可阻止停止并回灌（我们不用
  block，只写 marker）；注意 `stop_hook_active` 与连续 block 上限防循环

已知噪声：社区报告 Stop hook 偶有触发过频（如等待审批时）。

决策：
1. **主信号** = Stop hook 写 marker 文件（session_id + transcript_path + 时间戳）
2. **兜底** = JSONL mtime 静默阈值 + 硬超时双保险（防 hook 缺配/失灵）
3. 引擎启动时确保 Stop hook 已配置（写入 ~/.claude/settings.json，幂等）

## Spike C：claude --resume 跨目录 + 审批模式 —— ✅ CLI 层成立（API 层未验）

实测（claude 2.1.238 原生 exe，从 `/tmp` 跨盘符目录 resume
`F--project-workspace-side-my-skils` 项目的会话）：

- `--resume <session-id>` 按会话 ID **全局**定位，与当前目录无关：跨目录
  加载成功、session_id 正确回显、推进到 API 调用阶段
- API 层因账户 402（余额不足）无法端到端验证——**部分验证**，CLI 层结论可靠
- `--permission-mode <mode>` 与 `--dangerously-skip-permissions` 均存在；
  阶段 2 工作 pane 通过 args 透传（6ccb3d2）挂免审批

**意外发现**：本机 npm 全局 claude 处于半升级损坏态——`bin/claude.exe` 是
占位 stub、`claude-code-win32-x64/` 里只剩被改名的 `claude.exe.old`（330MB
真身）、新版躺在 `.claude-code-SFgM2ooi` 残留目录。launch.rs 的残留目录扫描
（find_claude_native_exe 第 3 候选）实测命中该场景。

## 阶段 2 设计决策（依据以上结论）

1. 引擎进 Rust（拿得到 PTY writer 与进程树管理，ps1 只保留审查段）
2. 轮次状态机：注入任务 → Stop marker（主）/静默超时（兜底）→ codex 审查
   → 未过则注入回灌 → 循环，硬上限 + 收敛判停
3. 注入前校验 pane 的 work_dir 仍等于任务目录（用户改目录即暂停告警）
4. 工作 pane 启动参数带 `--dangerously-skip-permissions`，防止干活中途卡审批
