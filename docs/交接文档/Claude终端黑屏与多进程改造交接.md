# 交接文档：Claude 终端黑屏 + 多进程监督改造

> **给下一任 agent / 接手的人读。**  
> 写文档目的：把「已经试过什么、证伪了什么、现在卡在哪、代码在哪条分支」一次说清楚，避免再从零猜。  
> **不要再拿「首帧 orphan 丢失」当已证实根因继续改前端**——用户已否定该路径（见下文硬事实）。

> **★ 最新状态（2026-08-26）：黑屏已定位并修复。** 根因不是前端/orphan/多 pane，而是**本机 Claude Code 原生二进制损坏**（自动更新失败所致），已还原二进制并修好前端「CLI 秒退仍卡 running」竞态。完整证据链、修复步骤与「为什么官方更新会失败」见文末 **§12**。

---

## 0. 一句话现状

1. **产品改造（同空间多监督任务、一任务一 Claude PTY）**：代码主要在分支 `wip/multiproc-claude-terminal` 的提交 `d28a661`，**未合回 main，未 push**。  
2. **真机阻塞 bug**：终端工作台里 Claude 显示「运行中」、页脚有 `terminal-N`，画面全黑；**同屏 Codex 正常**。  
3. **最新硬事实（2026-08-26）**：黑屏时 **任务管理器里没有 Claude 相关进程**。  
   → 不是「进程在跑但画面丢了」，而是 **UI 以为启动成功，本机进程根本没起来（或瞬间没了且我们没看见）**。  
   → 之前「PTY 首帧在 session 绑定前被丢弃」的修复 **不能解释这个现象**，用户实测仍黑屏。

---

## 1. 用户要的产品目标（不要搞错）

对齐 Codex 两级模型，且必须保留「驱动 Claude 终端、人可插手」：

| # | 要求 | 备注 |
|---|------|------|
| 1 | 同空间可开多个监督任务 | 独立状态 / 可分别取消 |
| 2 | 切空间时任务列表、终端、审查看板不串台 | |
| 3 | 真并发 = **一任务一 Claude PTY（多进程）** | 用户明确：**不要设进程上限**，要测极限 |
| 4 | 做完后要子代理审查代码 | 曾做过一轮，结论 Request Changes，部分已修 |

产品决策（已拍板）：同空间可同时跑 + 列表分别看。  
旧计划里「同目录串行 / `engine_busy_dirs`」已被推翻，应改为按 **terminal session** 互斥，同目录可多开。

相关计划文：`docs/计划/2026-08-26-工作空间多任务改造实施计划.md`（D3 已改成多进程表述，后半可能仍有旧 `engine_busy_dirs` 残留，以代码为准）。

---

## 2. Git / 工作区状态（接手第一件事）

### 2.1 分支

| 项 | 值 |
|----|-----|
| 当前分支 | `wip/multiproc-claude-terminal` |
| 备份完整多进程改动的 tip | `d28a661` — `wip(supervise): 备份多进程 Claude 与终端改造以便二分黑屏` |
| 备份点的父提交（二分对照基线） | `811e7ac` — `docs(handoff): 补充工作空间多任务改造执行方案` |
| 相对 `origin/main` | 本地曾 **behind 13**（开 WIP 分支前）；**不要假设已与远程同步** |

### 2.2 `d28a661` 里装了什么（完整备份，一次提交）

约 13 个文件，含：

- **后端**：`SuperviseRequest.terminal_session_id`；`engine_busy_dirs` → `engine_busy_sessions`；产物 `.supervise/tasks/<task_id>/`；Stop 注入令牌 `[supervise-task:<id>]` 预钉 session；`engine_hook_refs` 等  
- **前端**：多 Claude pane / `startClaudeForTask` / `prepareDriveTerminal`；ReviewBoard 无 `taskId` 不读盘；相关测试  
- **计划文档** 小改  

文件清单以 `git show --stat d28a661` 为准。

### 2.3 当前工作区（相对 `d28a661` 的脏改动）

为排查黑屏做过 **二分回退 + orphan 补丁**，工作区 **不等于** `d28a661`：

| 文件 | 相对 `d28a661` 的状态 | 含义 |
|------|----------------------|------|
| `src/components/TerminalWorkspace.tsx` | 改回接近 `811e7ac` 的单 pane，再加了 orphan 缓冲 | **黑屏仍在** |
| `src/components/__tests__/TerminalWorkspace.test.tsx` | 同上 + orphan 回归测试 | 单测绿，**解释不了真机无进程** |
| `src/App.css` | 回退到 `811e7ac` | 去掉多 pane 相关样式 |
| `src/App.tsx` | 回退到 `811e7ac` | **已去掉** `prepareDriveTerminal` 接线 |

**后端多进程改动仍在 `d28a661` tip 上**（工作区未再改 Rust 的话，与 tip 一致）。  
以 `git status` / `git diff d28a661` 为准，不要凭记忆。

### 2.4 如何恢复「完整多进程备份」工作区

若要丢掉黑屏实验、回到备份提交内容：

```powershell
cd F:\project\workspace-side\Harness_agent
git checkout wip/multiproc-claude-terminal
git reset --hard d28a661
```

⚠️ `reset --hard` 会丢掉当前未提交的 orphan/回退实验。用户未授权前不要擅自 hard reset；本文只说明路径。

若只要恢复某一个文件：

```powershell
git checkout d28a661 -- src/components/TerminalWorkspace.tsx
```

---

## 3. Claude 黑屏：现象必须写准

### 3.1 UI 表现（用户截图 / 口述一致）

- 页：终端工作台；左右 Claude | Codex。  
- **Claude**：状态绿点「运行中」；页脚有 `terminal-1`（或类似 id）；**画面全黑**（有时仅左上角光标，有时连光标都没有）。  
- **Codex**：同目录下可正常出 OpenAI Codex TUI（可能有无关 MCP 报错，与 Claude 黑屏无关）。  
- 底栏/侧栏「Claude 50」是 **会话列表条数 + 品牌色**，**不是**终端健康状态。不要当成「Claude 挂了」的依据。

### 3.2 关键硬事实（证伪一批前端猜测）

| 观察 | 含义 |
|------|------|
| 回退多 pane / 多 tab 前端后 **仍黑** | **不能**把根因归咎于「多 Claude tab / display:none / 多 pane CSS」 alone |
| 加 orphan 缓冲 + 启动同步 reset 后 **仍黑** | **不能**把根因说成「已证实是 session 绑定前丢欢迎屏」 |
| **任务管理器无 Claude 进程** | UI「运行中」≠ 本机 Claude 在跑；优先查 **spawn 是否成功、找没找对 exe、进程是否秒退、前端是否误标 running** |

复现目录示例（用户侧）：

`F:\project\workspace-side\Amazing\吴兆国-个人名片`

（路径含中文；Codex 能开，说明「仅因中文路径」不是充分解释，但仍可能是 Claude 启动链路上的差异因子。）

### 3.3 验证要求（之前约定过的）

- 必须 **整应用重启**（Tauri 全重启），不要只靠 Vite HMR。  
- 验收「画面好了」的标准曾定为：Claude **完整 TUI + 可打字**，Codex 仍正常。  
- **当前结果：未通过**；且新增「无后台进程」。

---

## 4. 已经试过什么（按时间，别重复劳动）

### 4.1 多进程实现过程中

- 做过 ClaudePaneGroup、多 `TerminalPane`、orphan 输出缓冲、去掉 effect 里晚到的 `reset` 等。  
- 用户反馈：**怎么改前端，Claude 终端都打不开 / 黑屏**。  
- 子代理 code-review 曾对多进程方案提 Critical（令牌子串、预钉失败串台等），部分已在备份提交前修过——**与黑屏是否同一 bug 未证明**。

### 4.2 黑屏二分（grilling 后执行）

| 步骤 | 结果 |
|------|------|
| 新建分支并 commit 备份 `d28a661` | 成功 |
| 工作区把 `TerminalWorkspace` + 测试 + `App.css` + `App.tsx`（去掉 prepareDriveTerminal）回退到 `811e7ac` | 用户：**仍黑屏** |
| 在回退版上只加 orphan 缓冲 + 启动时同步 reset + 单测 | 单测通过；用户：**仍黑屏，且任务管理器无进程** |

**结论（给接手人）：**  
前端「多 pane 引入黑屏」假设 **已否**。  
「仅 orphan 竞态」假设 **已被真机否**（至少作为**充分**解释不成立）。  
下一刀应落在 **Rust/Windows 启动 Claude 的路径、返回给前端的 session 语义、以及进程是否真实存活**，而不是继续堆 xterm 补丁。

### 4.3 曾被当成高嫌疑、但不得再当「已证实」的说法

以下在代码上「说得通」，**但与「无进程」冲突或已被回退实验削弱**：

1. `terminal-output` 在 `session.id` 绑定前丢弃 → Claude 欢迎屏只画一帧 → 黑屏。  
2. `starting` 的 `terminal.reset()` 晚于回放 → 清成黑屏。  
3. 多 Claude tab `display:none` / 尺寸为 0 → xterm 黑。  

可以保留为**次要/并发问题**，但 **当前用户报告的主症状不是它们能单独解释的**。

---

## 5. 启动链路：接手人该从哪读代码

不要漫无目的搜全库。按这条链读：

| 层 | 位置 | 看什么 |
|----|------|--------|
| 前端点启动 | `src/components/TerminalWorkspace.tsx` → `handleStart` → `startTerminal` | `status: starting → running` 的时机；**running 只表示 invoke 成功返回 session** |
| IPC | `src/lib/terminalApi.ts` | `invoke("start_terminal", …)` |
| Tauri 命令 | `src-tauri/src/lib.rs`（`start_terminal` 一带） | 校验 cwd、组 command、`spawn_terminal_pty`、插入 session、返回 `status: "running"` |
| Windows 找 Claude | `src-tauri/crates/terminal_host/src/launch.rs`（或同 crate 下 launch） | **Claude 走原生 `claude.exe`**；Codex 走 `cmd.exe /c codex`——两边不对称 |
| PTY 读线程 | `terminal_host` + `lib.rs` emit `terminal-output` | 有 session 无进程时，这里应根本没有持续输出 |

本机曾观察到的 Claude 安装路径（**仅供参考，接手时用 `where.exe claude` 再确认**）：

`F:\develop_soft\IDE\npm-data\global\node_modules\@anthropic-ai\claude-code\bin\claude.exe`

仓库内另有关于 stub / leftover 大 PE 的 spike/历史修复（如改走真实原生 exe）——若 spawn 指到坏 stub，可能出现「奇怪失败」；**需用当前机器实测，不要抄旧结论当真理**。

---

## 6. 下一任建议排查顺序（可执行，别空谈）

用户已说「不用继续分析」——下列是 **交接建议**，不是本会话未完成的分析结论。

1. **黑屏当场核对进程**  
   - 任务管理器 / PowerShell：`claude.exe`、`node` 带 claude 的命令行、父进程是否为 Harness。  
   - 若确认 **始终无进程**：优先查 `start_terminal` 为何仍返回成功 session（或前端是否用了陈旧 session id）。

2. **看 Tauri/Rust 日志与 `terminal-error` / `terminal-exit`**  
   - UI 是否订阅到了 exit/error 却仍显示 running。  
   - spawn 失败是否被吞。

3. **对比 Codex 启动**  
   - 同一 UI、同一台机器、Codex 有进程且有 TUI。  
   - 差分应落在 `launch.rs` 的 Claude 分支，而不是 TerminalWorkspace 布局。

4. **命令行手工启动**  
   - 在同一工作目录下直接跑本机 `claude`，确认 CLI 本身能出 TUI。  
   - 若 CLI 本身挂：先修安装/PATH，别改 Harness。

5. **多进程产品代码**  
   - 黑屏未解前，**不要**把「再改一版多 pane UI」当成黑屏修复。  
   - 多进程功能以 `d28a661` 为备份；黑屏修好后再 `merge/rebase` 并补 orphan 等防御（若仍需要）。

---

## 7. 多进程改造：备份里已有能力（供功能续作）

以下描述的是 **`d28a661` 意图**，续作前用 `git show d28a661` 核对：

- 同目录多任务：busy 按 **terminal session**，不按目录。  
- 引擎产物：`.supervise/tasks/<task_id>/`，禁止回退到空间根混写。  
- 驱动终端：前端 `prepareDriveTerminal` → `startClaudeForTask` → 把 `terminal_session_id` 传给监督请求。  
- ReviewBoard：没有选中 `taskId` 时不读盘，避免串台/空读。  
- Stop 预钉：注入 `[supervise-task:<id>]`，避免 `task-1` / `task-10` 子串误匹配。

**当前工作区**已临时拆掉 `App.tsx` 的 `prepareDriveTerminal`，监督「自动起 Claude」在二分期间是关掉的；续作多进程时要接回去。

---

## 8. 环境与注意

- 仓库：`F:\project\workspace-side\Harness_agent`  
- Windows；Cargo 常用：`F:\develop_soft\IDE\AI_tools\Rust_env\cargo\bin`  
- 检查编译曾用 `CARGO_TARGET_DIR=target-check`（已 gitignore）  
- 含中文的 Rust 源文件：勿用会搞坏编码的编辑方式；曾因此修过损坏文件  
- 提交规范：中文 Conventional Commits；不要加 AI co-author；**未经用户明确要求不要 push**  
- 前端设计/创意任务需走项目内 brainstorming skill；本文是交接，不是新功能设计

---

## 9. 明确不要做的事

1. **不要**再写「已定位为 orphan 首帧丢失」这类定论文档或 PR 描述——真机已否。  
2. **不要**在未确认有 `claude` 进程的情况下，继续大改 xterm / CSS / 多 tab 布局「赌一把」。  
3. **不要**在 `main` 上直接堆 WIP；续作继续用 `wip/multiproc-claude-terminal` 或由其拉新分支。  
4. **不要** `git pull`/`merge origin/main` 时假定无冲突——本地曾落后远程多提交。  
5. **不要**把「Claude 50」红/珊瑚色当成终端故障指示。

---

## 10. 交接检查清单（接手人打勾）

- [ ] `git branch` / `git log -3` / `git status` 已看，知道自己在 `wip/multiproc-claude-terminal` 还是别的分支  
- [ ] 已理解：`d28a661` = 多进程完整备份；工作区可能有黑屏实验脏文件  
- [ ] 已理解：黑屏 + **无进程** 是当前主阻塞  
- [ ] 已理解：前端二分与 orphan 补丁 **未解决** 真机问题  
- [ ] 已用本机 `where.exe claude` / 直接跑 CLI 确认安装是否正常  
- [ ] 再改代码前，先能回答：黑屏时 `start_terminal` 返回了什么、进程列表里有没有 Claude  

---

## 11. 相关文档索引

| 文档 | 用途 |
|------|------|
| `docs/计划/2026-08-26-工作空间多任务改造实施计划.md` | 多任务/多进程计划（注意旧 D3 残留） |
| `docs/交接文档/工作空间多任务改造执行方案.md` | 更早的执行方案交接 |
| `docs/交接文档/工作空间改造接续执行指南.md` | 工作空间改造接续 |
| `docs/交接文档/工作空间现状与Codex前端逻辑差距.md` | 与 Codex 两级模型差距 |
| 本文 | **Claude 黑屏 + 多进程备份分支** 的当前阻塞与证伪记录 |

---

**文档日期**：2026-08-26  
**最后实测结论（2026-08-26 已推翻）**：Claude 终端黑屏已定位为「本机 Claude Code 二进制损坏」并修复；此前「未修复 / 继续改前端画面层」的结论不再成立，详见下文 §12。

---

## 12. 黑屏根因定位与修复（2026-08-26 已解决）

> 本节推翻 §0/§3 的「未修复」结论。根因不在 Harness 前端，在本机 Claude Code 安装。

### 12.1 根因

Harness spawn 的 `claude-code\bin\claude.exe` 是**损坏的合法 PE**：MZ/PE 头、版本资源（2.1.245）都完好，所以 `CreateProcessW` 能成功、`spawn_terminal_pty` 不报错；但入口代码被写坏，运行后**秒退 exit 0、零输出、零子进程**。

触发源：`C:\Users\admin\.claude\.last-update-result.json` 记录 2026-08-26 自动更新 `2.1.245 → 2.1.246` 失败（`install_failed` / `update_apply_restore_failed`），把二进制留在了半写坏状态。

### 12.2 定位证据（本机实测）

| 检查 | 结果 |
|------|------|
| `where.exe claude` | 命中 `claude` / `claude.cmd` shim（无 `claude.exe`，与 §5 旧笔记不同，需以当前机器为准） |
| 三个二进制的 `--version` | `bin\claude.exe`（被 spawn）：无输出、exit 0、秒退无子进程；`.claude-code-SFgM2ooi` 残留：`2.1.245 (Claude Code)` ✅；`claude.exe.old.…`（2.1.236）：无输出 |
| SHA256 | 被 spawn 的 `F04BFB87…` ≠ 残留 `D1649BF5…`（两者同为 384,213,664 字节、同为 2.1.245 → 是内容被写坏，不是版本差异） |
| 平台包 `@anthropic-ai/claude-code-win32-x64` | **空目录**：只剩 `claude.exe.old.…`（2.1.236），无 `claude.exe`、无 `package.json` |
| 注册表 / 磁盘 | `npm view @anthropic-ai/claude-code-win32-x64@2.1.246` 正常返回；F 盘剩 250GB |

### 12.3 完整解释「黑屏 + 无进程 + 运行中」

1. `start_terminal` 找到坏 exe，`spawn_command` 对合法 PE 成功 → 返回 `status:"running"`。
2. 坏 exe 秒退 → 无进程、无输出 → 黑屏。
3. `terminal-exit` 来得比前端绑定 `session.id` 更早，`TerminalWorkspace.tsx` 的 exit 处理循环匹配不到 session → 事件被丢 → UI 永远停在「运行中」。

（§4 里「孤儿首帧 / 多 pane / CSS」等假设，都是这条主因之外的次要/并发现象，被「无进程」硬事实证伪为**非充分解释**。）

### 12.4 修复

1. **还原二进制**：坏文件先备份为 `claude-code\bin\claude.exe.corrupt.20260826.bak`，再用残留可用 2.1.245 覆盖；`claude --version` / `--help` 恢复输出，SHA256 与残留一致。
2. **前端竞态**：`src/components/TerminalWorkspace.tsx` 新增 `orphanExitRef`，缓冲「早于 session 绑定的 terminal-exit」，`handleStart` 绑定后立即应用为 `exited`（异常码显示「CLI 异常退出（代码 N）」），不再卡 running。补 2 个回归测试；全量 158 测试 + `tsc --noEmit` 通过。

### 12.5 为什么官方更新会失败（机制，供以后再遇到时判断）

- **非原子替换**：npm 壳包 `install.cjs` 的 `placeBinary` 是 `unlinkSync(旧 bin/claude.exe)` → `linkSync/copyFileSync(平台包新二进制)`。删旧之后任何中断 = 空/坏二进制；「回滚」只是再跑一次 `npm install -g @anthropic-ai/claude-code@旧版`，同样可能失败 → `update_apply_restore_failed`。
- **本次触发（临时性，非持久故障）**：2.1.246 原生二进制解压约 250MB，走 `registry.npmjs.org` 下载；平台包最终落空 = 下载/解压被临时打断；或运行中的 `claude.exe` 文件锁阻止替换（Harness 常驻 spawn claude，可能性高）。**已排除**磁盘满、注册表/鉴权持久故障。
- **为何难发现**：坏二进制仍是合法 PE，`start_terminal` 不报错，前端只看到「invoke 成功」，于是被误判为前端渲染问题。

### 12.6 复发预防（用户明确不能关自动更新）

1. 更新窗口期**确保没有 Claude 进程**（先关 Harness 里的 Claude 终端）。
2. 再遇 `install_failed`：在无进程时手动 `claude update` 或 `npm install -g @anthropic-ai/claude-code`。
3. 大包下载频繁断流可给 npm 配国内镜像（本机缓存里已有 npmmirror 记录）。

### 12.7 遗留

- 多进程产品续作仍按 §2.4 从 `d28a661` 恢复（`App.tsx` 的 `prepareDriveTerminal` 仍是拆掉的）。
- `terminal-error` 与 `terminal-exit` 同类的秒退竞态**未修**（罕见：仅 PTY read 报 Err 才触发；本次坏二进制走的是 EOF → exit）。
- 坏二进制备份 `claude.exe.corrupt.20260826.bak` 确认无碍后可删。
