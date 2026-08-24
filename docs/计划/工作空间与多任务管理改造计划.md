# 工作空间与多任务管理：学习 Codex 的改造计划

> 目标：把监督闭环从"全局单目录 + 单任务"升级为 Codex 同款
> **工作空间（一级实体）→ 每空间多任务（并发、互不干扰）** 的两级模型。
> 依据：对 `F:\project\workspace-side\Amazing\codex`（codex-rs）源码的深度分析。

## 一、Codex 的设计精髓（源码证据）

1. **工作空间（Project）是一级持久化实体，独立于任务**
   `Project{id, name, roots[], metadata, position}`（`state/src/model/project.rs:6-20`），
   存 SQLite（`state/migrations/0049_projects.sql`），支持排序（position）、
   幂等创建、多根目录。**不在 config.toml**——config 里的 `[projects]` 只是信任表。

2. **任务（Thread）从属于工作空间，目录绑定在线程上**
   `threads.project_id` 可空外键 `ON DELETE SET NULL`（删空间不删任务，
   任务变"未分配"）；线程 cwd 不可变，**切目录 = fork 新线程 + 归档旧线程**
   （`tui/src/app/working_directory.rs:119-135`）。

3. **并发模型：多线程并存、线程内串行、线程间并发**
   `ThreadManager.threads: RwLock<HashMap<ThreadId, Arc<CodexThread>>>`
   （`core/src/thread_manager.rs:336-359`）；请求按 `Thread{id}` 作用域串行
   （`app-server-protocol/src/protocol/common.rs:119-197`）。**不用 git worktree，
   隔离 = 每线程独立 rollout JSONL + 每线程 writer 锁。**

4. **两级导航数据通路**
   `project/list` → `thread/list?projectId=...`（双 Option：null=未分配）；
   变更靠 `project/changed`、`thread/status/changed` 推送。
   桌面端侧栏 = 空间列表（可拖排序 `project/move`），主区 = 该空间任务列表。

5. **任务状态机**
   本地线程：`NotLoaded / Idle / Active{WaitingOnApproval|WaitingOnUserInput}
   / SystemError` + archived 归档态（`protocol/v2/thread.rs:1621-1642`）。

6. **信任确认是添加空间流程的一部分**
   首次添加目录弹信任对话框（信任 git 根而非子目录，`tui/src/onboarding/
   trust_directory.rs`），持久化 trust_level，未信任目录拒绝切换。

## 二、harness_agent 现状差距

| 维度 | 现状 | 差距 |
|------|------|------|
| 工作目录 | 单个 `projectWorkDir`（localStorage 字符串） | 无实体、无列表、无多空间 |
| 任务并发 | 后端 `busy_workdirs` 已支持**不同目录**各跑一个；前端表单只绑一个目录、`runningTask` 单值 | UI 只能看到/启动一个任务 |
| 任务可见性 | 无任务注册表：运行中任务只存在于面板局部 state，重启即失忆 | 无任务列表/历史/状态机 |
| 看板绑定 | `superviseDir` 记最后启动的目录（单值） | 无按空间查看 |
| 信任 | 无概念 | 首次添加无确认 |

## 三、适配映射（Codex 概念 → harness 实现，保守取舍）

| Codex | harness 采纳 | v1 保守取舍 |
|-------|--------------|------------|
| Project 实体（SQLite） | `Workspace{id, path, name, position, trusted}` 列表，localStorage 持久化 | 不引 SQLite——空间量小、无复杂查询 |
| Thread 并发 HashMap | 后端**已有** `busy_workdirs`（每目录一任务）；新增任务注册表 `HashMap<task_id, TaskInfo>` | 同空间仍单任务（与 Codex 线程内串行一致）；跨空间并发 |
| 状态机 NotLoaded/Idle/Active | `TaskStatus: Running / Accepted / Rejected / Cancelled / Aborted`（复用 EngineStatus + ps1 退出码） | 不做 WaitingOnApproval 细分（我们免审批注入） |
| thread/list + 变更推送 | 新 command `list_supervise_tasks` + 复用 `supervise-done` 事件更新 | 轮询式读取（切 tab / 事件后刷新），不做增量推送 |
| 切目录=fork 线程 | 不做——引擎已有 pane 绑定校验（目录被换即中止），终端会话目录保持不可变 | 未来项 |
| 信任对话框 | 添加空间时确认对话框（说明"该目录将被注入任务/写入产物"），确认即 trusted=true | 信任=一次性确认，不映射沙箱档位 |
| 排序拖拽 | v1 按添加顺序 + 预留 position 字段 | 拖拽排序后续 |
| follow-up queue | 不做 | — |

## 四、分阶段 TODO（每阶段独立提交 + 测试）

### 阶段 A：工作空间数据模型（前端）
- [ ] `src/lib/workspaces.ts`：Workspace 类型 + localStorage CRUD
      （list/add/remove/setActive/rename，position 保序）
- [ ] 迁移：旧 `ha-project-work-dir` 非空 → 自动成为第一个空间
- [ ] App 持有 `workspaces` + `activeWorkspaceId`；`projectWorkDir` 派生自
      active workspace（终端 Claude pane / 监督表单 / ReviewBoard 现有链路不动）
- 验收：单测（CRUD/迁移/激活切换）+ 既有 92 前端测试全绿

### 阶段 B：后端任务注册表（多任务地基）
- [ ] `SuperviseState.tasks: Mutex<HashMap<String, TaskInfo>>`；
      `TaskInfo{id, work_dir, kind(Engine|Ps1), status, rounds, last_reason, started_at_ms}`
- [ ] `run_supervise` / `run_supervise_terminal` 启动时登记 Running；
      结束线程（ps1 收尾 / 引擎收尾）按 outcome 更写终态
- [ ] 新 command `list_supervise_tasks` → Vec<TaskInfo>（含历史终态，
      应用退出即清——v1 不落盘，重启后历史消失属预期）
- 验收：引擎/ps1 既有 Rust 测试全绿 + 注册表状态流转单测

### 阶段 C：监督页两级 UI（Codex 布局）
- [ ] 监督闭环 tab 重构：左侧空间栏（列表+添加+移除+激活态），
      右侧 = 激活空间的 [任务列表（运行中+历史） + 启动表单 + 审查看板]
- [ ] 任务列表行：状态徽标（Running/Accepted/Rejected/…）、轮数、目录、
      开始时间；Running 行带"取消"（复用 cancel_supervise）
- [ ] 表单目录 = 激活空间目录（只读展示，换目录=切空间——Codex 同款心智）
- [ ] ReviewBoard 绑定激活空间目录（替换 superviseDir 单值）
- [ ] StatusBar supervising 计数改接任务注册表实况（≥0）
- 验收：组件测试（空间切换/任务列表渲染/取消调用）+ 全量回归

### 阶段 D：信任确认与打磨
- [ ] 添加空间流程：选目录 → 确认对话框（Codex trust 文案精神）→ 入列表
- [ ] 空间重命名（name 可编辑，默认目录 basename）
- [ ] README/文档更新；发布构建验证
- 验收：全量测试 + tauri build 出包

## 五、明确不做（本轮边界）

- 同一空间内多任务并发（Codex 语义也是线程内串行；我们每空间一任务）
- SQLite 持久化、任务跨重启历史、thread queue、fork-on-cd、拖拽排序
- 终端工作台双 pane 结构不动（Claude pane 跟随激活空间，Codex pane 维持独立）
