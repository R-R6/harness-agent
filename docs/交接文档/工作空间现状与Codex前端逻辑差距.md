# 交接文档：工作空间功能现状与 Codex 前端逻辑差距（供 GPT 接手）

> 交接对象：GPT（下一任 agent）。目的：接手工作空间/多任务功能，对齐 Codex
> 应用端前端逻辑。**本文只做现状与差距的诚实陈述，不重复计划与提交细节**——
> 那些请看 `docs/计划/工作空间与多任务管理改造计划.md`、`docs/交接文档/工作空间改造接续执行指南.md` 和 git log。
>
> 提交范围：`39ba619..2a4e49e`（工作空间与多任务改造全部 8 个提交）。

## 一、我对 Codex 前端逻辑的理解

Codex 应用端是**两级模型**：

1. **工作空间（Project）是一级实体**：可增删、排序、激活，持久化。
2. **每个工作空间下有多个 Thread（对话/任务）**：thread 从属于 project，
   切空间 → 显示**该空间自己的 thread 列表**；同一空间可并发开多个 thread
   （thread 内串行、thread 间并发）。
3. **对话与任务是一对一绑在 thread 上的**，cwd 不可变，换目录 = 换空间/开新 thread。
4. 关键心智：**空间之间彻底隔离**——A 空间的对话列表、任务列表、审查看板，
   切到 B 空间后应完全换成 B 的内容，不串台。

## 二、我实际实现了什么（诚实清单）

| 项 | 实现情况 | 位置 |
|----|---------|------|
| 空间实体 | ✅ localStorage 列表，增删/切换/重命名/迁移 | `src/lib/workspaces.ts` |
| 添加空间 | ✅ 信任确认对话框 + 追加语义（不覆盖） | `src/App.tsx` `handleAddWorkspace` |
| 后端任务注册表 | ✅ 内存 HashMap，含 work_dir/status/rounds 等 | `src-tauri/src/lib.rs` `TaskInfo` |
| 任务列表 UI | ⚠️ **全局任务列表，未按激活空间过滤**（见问题 2） | `src/components/TaskList.tsx` |
| 左侧空间栏 | ✅ 列表+切换+移除+重命名 | `src/components/WorkspaceSidebar.tsx` |
| 审查看板 | ✅ 按激活空间 path 加载（这个是对的） | `src/components/ReviewBoard.tsx` |
| 启动表单目录 | ✅ 只读，绑定激活空间 | `src/components/SupervisePanel.tsx` |

**两个覆盖 bug 已修复**（`ffe402b`、`2a4e49e`）：添加空间和终端目录上报都从
"改目录覆盖"改成了"追加新空间"，`resolveDirChange` 已删除。

## 三、与 Codex 的差距（用户已指出的问题，必须优先修）

### 问题 1：同一个工作空间不能开多个新任务/对话

**现状**：后端 `busy_workdirs` 锁按 **work_dir 互斥**（同目录只能跑一个任务），
前端启动表单只有一个 `runningTask`。这是 `docs/计划/工作空间与多任务管理改造计划.md` 第五节**明确"不做"**
的 v1 边界（"同一空间内多任务并发——不做；每空间一任务"）。

**差距**：Codex 是"每空间多 thread 并发"。要实现同款，需：
- 后端把互斥锁从"按 work_dir"改为"按 task_id"（或放宽为每空间 N 任务上限）；
- 前端启动表单支持"同空间再开一个任务"，任务列表按 task_id 区分（不是按目录）。

### 问题 2：不同空间点击显示同一个对话、同一个监督任务（**实现缺陷，非边界**）

**根因（已确认，代码级）**：`src/App.tsx:694` 的
`<TaskList tasks={tasks} onCancel={handleCancelTask} />` 传的是 `fetchTasks()`
拉到的**全局任务列表**，**没有按 `activeWorkspace.path` 过滤**；计数
`count-pill`（:692）也是全局 `tasks.length`。所以切到 B 空间，任务记录里
仍显示 A 空间的任务。

**对话（终端 pane）同理**：`TerminalWorkspace` 是单实例，切空间只改
`projectWorkDir` 这个 prop（Claude pane 目录输入框会更新），但**已启动的 PTY
会话不会重建**——切空间后看到的还是同一个终端对话（此点为代码推断，未真机实测）。

**正确做法（对照 Codex）**：
- 任务列表改为 `tasks.filter(t => samePath(t.work_dir, activeWorkspace.path))`；
- 终端 pane 需按空间隔离（每个空间独立会话，或切空间即提示"当前对话属于别的空间"）。

## 四、当前验证状态（诚实）

- 前端测试 **141 例全绿**（vitest）、`tsc --noEmit` 0 错误；
- Rust 测试 59 例全绿、`cargo clippy` 0 警告；
- 发布包已构建：`src-tauri/target/release/harness_agent.exe`（0.2.0）。
- **未覆盖**：问题 2 的"任务列表按空间过滤"无专门测试；终端 pane 空间隔离无测试。
- 上述测试全绿**不代表功能符合 Codex 逻辑**——只证明我没把已写的代码写崩。

## 五、建议 GPT 接手后优先做

1. 通读 `docs/计划/工作空间与多任务管理改造计划.md`（尤其第五节"明确不做"——问题 1 正是此处取舍）+
   本文 + 问题 2 的根因。
2. **先修问题 2**（实现缺陷）：任务列表按激活空间过滤 + 终端对话按空间隔离。
3. 再评估问题 1：是否要突破 v1 边界做"同空间多任务并发"（后端锁 + 前端表单都要改）。
4. 每步配测试 + 分步提交（仓库惯例：中文 Conventional Commits，禁 AI 署名尾注）。

## 六、建议技能（suggested skills）

- `karpathy-guidelines`：用户偏好保守、手术式改动、先复现再修、可验证完成。
- `code-review`：改动后用 Standards + Spec 双轴审查（Spec 轴曾抓到覆盖 bug 的第二入口）。
- `tdd`：每行为改动配回归测试（先写复现用例再修）。
- `commit`：中文 Conventional Commits，main 直推（用户已授权），禁 AI 署名。

## 七、关键文件速查（不重复内容，仅索引）

- `docs/计划/工作空间与多任务管理改造计划.md` — 四阶段计划 + Codex 设计证据 + 明确不做清单
- `src/lib/workspaces.ts` — 空间数据模型 + `addWorkspace`（追加语义）
- `src/App.tsx` — 状态派生（:112-116）、目录上报（:118-124）、任务列表接线（:688-695）
- `src/components/TaskList.tsx` — 任务列表渲染（**未过滤，待修**）
- `src/components/WorkspaceSidebar.tsx` — 空间侧栏
- `src-tauri/src/lib.rs` — 任务注册表 `TaskInfo` + `list_supervise_tasks` + `busy_workdirs` 锁
