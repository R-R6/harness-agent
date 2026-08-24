# LangChain / LangGraph 对 Harness Agent 的技术参考性评估

日期：2026-08-15  
范围：仅评估架构参考性；本次不修改应用代码、不引入依赖。  
对象：Harness Agent（Tauri 2 + React/TypeScript + Rust），包含会话浏览、监督闭环、MCP 检查，以及规划中的 Claude/Codex 本机 CLI PTY 终端工作台。

## 结论先行

LangChain 有参考性，但不适合作为 Harness Agent 的终端宿主或第一阶段的核心依赖。推荐“局部采用、接口预留”：

1. 继续由 Rust/Tauri 负责本机进程、PTY/ConPTY、窗口尺寸同步、输入输出和进程树回收。
2. 继续把现有 `supervise_runner` 的显式轮次、PowerShell 调度和 `.supervise` 产物作为第一阶段监督闭环的事实来源。
3. 为监督引擎抽象出状态图/事件接口；未来在需要跨重启恢复、人工审批、动态分支或多 Agent 编排时，引入 LangGraph（优先考虑 LangGraph.js sidecar）替换或增强监督编排层。
4. 只在确有需要时使用 LangChain 的模型/工具抽象；不要用通用 `ShellTool` 直接暴露任意命令，也不要让 LangChain 取代现有会话 JSONL/MCP 数据源。

换句话说：LangChain/LangGraph 可以帮助“编排和监督”，不能替代“终端和本机 CLI 集成”。对于当前桌面端版本，最合适的决策是**不立即引入 LangChain，保留可替换的 LangGraph 接缝**。

## 官方资料与关键事实

下表只列 LangChain/LangGraph 官方文档或官方仓库；事实与架构判断分开标注。链接是第一方来源，便于落地前按具体版本复核。

| 官方来源 | 可核对的事实 | 对 Harness Agent 的含义 |
| --- | --- | --- |
| [LangChain Python overview](https://docs.langchain.com/oss/python/langchain/overview) | LangChain 是用于构建由语言模型驱动应用的开源框架，提供模型、工具、检索和 Agent 等组合能力。 | 可作为 LLM/工具适配层，但不是桌面终端框架。 |
| [LangChain agents](https://docs.langchain.com/oss/python/langchain/agents) | 官方 Agent API 将模型与工具组合成循环式执行流程；Agent 会持续处理模型响应和工具调用直到结束。 | 可表达“执行—审查—返工”循环，但仍需要外部进程适配器、状态持久化和安全边界。 |
| [LangChain tools](https://docs.langchain.com/oss/python/langchain/tools) | 工具是带输入模式的可调用单元，可由模型请求调用。 | 可把“启动监督任务、读取审查产物、请求人工确认”等封装成显式工具；不能把工具权限默认扩大为任意 shell。 |
| [LangChain short-term memory](https://docs.langchain.com/oss/python/langchain/short-term-memory) | 短期记忆保存在 Agent 状态中，按 thread 组织，并可通过 checkpointer 持久化。 | 适合保存监督图的状态；它不是 Claude/Codex 本地 JSONL 会话的替代品，需要显式适配。 |
| [LangGraph overview](https://docs.langchain.com/oss/python/langgraph/overview) | LangGraph 是更底层的有状态 Agent 编排框架，强调长运行任务、持久化执行、人工介入、记忆和流式事件。 | 比高层 LangChain Agent 更贴合 Harness 的确定性监督状态机。 |
| [LangGraph durable execution](https://docs.langchain.com/oss/python/langgraph/durable-execution) | LangGraph 支持可恢复的长运行执行；恢复依赖持久化和可重放的节点设计。 | 可用于“应用崩溃后从第 N 轮继续”，但 PTY 和外部 CLI 仍需单独处理幂等、重连和进程存活。 |
| [LangGraph interrupts](https://docs.langchain.com/oss/python/langgraph/interrupts) | 图执行可以在人工确认处暂停并在之后恢复。 | 适合把危险命令、最终合并、预算超限等动作设为审批点。 |
| [LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence) | 图状态可由 checkpointer 按 thread 保存和恢复。 | 可持久化 supervisor 元数据；不应未经设计复制包含凭据或项目机密的终端输出。 |
| [LangChain 官方 Python 仓库](https://github.com/langchain-ai/langchain) / [LICENSE](https://github.com/langchain-ai/langchain/blob/master/LICENSE) | 官方仓库与许可证文件是 LangChain 源码及许可的第一方入口；当前仓库以 MIT 许可证发布。 | 代码层面通常可作为第三方依赖使用，但发布时仍需保留许可声明并核对实际锁定版本及传递依赖。 |
| [LangGraph 官方仓库](https://github.com/langchain-ai/langgraph) / [LICENSE](https://github.com/langchain-ai/langgraph/blob/main/LICENSE) | 官方仓库与许可证文件是 LangGraph 源码及许可的第一方入口；当前仓库以 MIT 许可证发布。 | 适合以独立编排组件评估，不等于获得 Claude/Codex 产品或 CLI 的许可。 |

> 版本提示：LangChain 文档和包名会随版本演进。上表链接是规范入口；实现前应在锁定版本上再次核对 API、许可证和依赖树。

## 与当前 Harness Agent 的匹配度

### 1. 本机 Claude/Codex CLI PTY 集成

**结论：不直接适合，必须由 Harness 自己实现。**

LangChain 官方能力围绕模型调用、工具调用和 Agent 编排；上述官方资料没有把 Windows ConPTY、PTY 字节流、ANSI 终端渲染、终端尺寸同步或进程树生命周期作为运行时职责。因此，把 Claude/Codex 当作“本机 CLI 子进程”时，建议分层如下：

```text
React 终端 pane
        │ 事件/输入
Tauri command + Rust PTY/ConPTY manager
        │ stdin/stdout/resize/exit
用户安装的 claude / codex CLI

可选：Supervisor engine（现有 Rust/PowerShell，未来 LangGraph sidecar）
```

LangChain 只能通过自定义工具或 RPC 调用这个终端/进程服务。它不会自动提供：

- ConPTY/PTY 创建、窗口尺寸变化和 ANSI/光标处理；
- CLI 登录、订阅、额度、会话恢复或 `claude --resume` / `codex resume` 语义；
- 子进程树清理、Windows Job Object、异常重连；
- 对本地 Claude/Codex JSONL 的安全读取。

因此，第四个“终端工作台”应继续按已确认的 PTY 设计实现，不能以 LangChain Agent 取代终端层。

### 2. 监督闭环

当前项目已有 `supervise_runner`：它启动 `supervise.ps1`，按显式轮次运行并解析 `review-N.md` / `final-report.json`，同时通过 `supervise-log` / `supervise-done` 向 UI 推送事件。这类流程是可预测的状态机，不需要先引入通用 Agent 循环。

LangGraph 在以下情况下有明显增益：

- 监督流程从固定轮次变成动态分支（按审查结果选择不同 Agent/工具）；
- 需要持久化 checkpoint，在崩溃或重启后继续；
- 需要把高风险操作、最终接受、合并或继续扣费设置为人工审批点；
- 需要统一流式事件、重试和多 Agent 子图。

LangGraph 不能替当前实现自动解决以下问题：

- 启动并托管真实 CLI 的 PTY；
- 确认 CLI 的输出已经写入正确 JSONL；
- 保证重复恢复不会重复执行不可逆命令；
- 将外部进程的退出、超时、取消、孤儿进程纳入桌面应用生命周期。

建议先定义与实现无关的 `SupervisorEngine` 接口（任务、轮次、事件、取消、产物），未来把现有 PowerShell/Rust 引擎与 LangGraph engine 都接在该接口后面。

### 3. 会话记忆

LangChain/LangGraph 的 thread state/checkpoint 适合保存监督器上下文，例如：任务 ID、当前轮次、审查结论、待人工确认的节点和重试计数。

它不应成为以下数据的第二个事实来源：

- Claude/Codex 原生会话 JSONL；
- agent-sessions-mcp 暴露的会话列表与正文；
- PTY 中可能包含 API key、环境变量、源码和用户输入的原始终端缓冲。

Harness 的会话浏览仍应通过现有 `session_proxy` → `agent-sessions-mcp` 链路；如果引入 checkpoint，只持久化最小化、可审计的监督元数据，并明确版本、加密和清理策略。

### 4. 工具调用

LangChain 的 typed tool schema 可帮助统一“读取审查产物”“请求人工确认”“查询 MCP 状态”等操作。但是 Claude/Codex CLI 本身已经拥有各自的工具和权限语义，外层再包一层通用 Agent tool loop 会产生嵌套循环和权限重复问题。

推荐的工具边界：

- 允许列表，而不是任意命令字符串；
- 每个工具声明工作目录、读写范围、超时、是否需要人工确认；
- 复用现有 Tauri command 的路径校验、MCP 白名单和任务互斥锁；
- 危险操作默认暂停，确认后再恢复；
- 工具返回结构化结果，原始 stdout/stderr 只在需要时以脱敏事件传给 UI。

## 引入成本与运行时选择

### 方案 A：继续 Rust/Tauri 编排（当前推荐）

- 不增加 Python/Node sidecar；
- 复用现有 `session_proxy`、`supervise_runner`、Tauri 事件和测试；
- 更容易保证 PTY、Job Object、取消和桌面退出的一致生命周期；
- 固定轮次监督逻辑透明，调试路径短。

代价是动态 Agent 图、checkpoints 和人工审批需要自行实现，但当前产品的闭环契约已经明确，代价可控。

### 方案 B：LangGraph.js 独立 sidecar（未来可选）

- 与当前 TypeScript/Node 生态更接近，避免捆绑完整 Python 运行时；
- 通过 stdio/本地 IPC 接收任务、发送结构化事件；
- PTY 仍由 Rust 服务掌管，LangGraph 只调 RPC/工具；
- 适合多 Agent、动态分支和可恢复审批流程。

新增成本包括 Node sidecar 的打包、版本锁定、启动/停止/崩溃回收、IPC 契约、依赖供应链和测试矩阵。不要让 sidecar 直接访问整个文件系统或继承全部环境变量。

### 方案 C：LangChain/LangGraph Python sidecar（不建议作为桌面首版）

Python 生态完整，但需要分发或检测 Python 运行时、安装依赖、处理虚拟环境与杀进程；这会把一个 Tauri 桌面应用变成多运行时产品。只有在团队已有稳定 Python 服务边界或计划把监督器独立部署时才值得考虑。

## 安全、合规与许可证边界

1. **依赖许可证。** LangChain 和 LangGraph 官方仓库的 LICENSE 入口标示 MIT；发布时仍须保留版权/许可文本，并对实际锁定版本运行 SBOM/license 检查。Provider adapter、数据库、终端库和传递依赖不能假定都使用 MIT。
2. **第三方 CLI 条款独立。** 使用 LangChain 不会改变 Anthropic Claude Code 或 OpenAI Codex CLI 的许可、服务条款、订阅和品牌规则。Harness 不应捆绑二进制、拦截隐藏协议、绕过登录/额度/风控或收集凭据；产品页面应明确“第三方本机 CLI 工作台，不代表官方背书”。
3. **凭据与遥测。** LangChain 本身是编排库；模型供应商 API、可选 LangSmith tracing 等外部服务有各自的数据路径。默认关闭外部 tracing，提供显式 opt-in，并在设置中说明数据去向；不把 PTY 输出和环境变量写入遥测。
4. **命令执行。** 不能把通用 shell 工具暴露给不受信任的 Agent 输入。会话正文、CLI 输出和仓库文件都可能包含 prompt injection；所有写文件、删除、提交、网络访问和继续执行操作都应经过 allowlist 与人工确认。
5. **恢复与幂等。** checkpoint 恢复可能重放节点。对启动 CLI、写文件、提交代码等副作用操作使用幂等任务 ID、结果记录和人工确认，避免重复执行。
6. **进程隔离。** PTY 进程和可选 sidecar 需要显式工作目录、最小环境变量、超时、取消和进程树清理；不能依赖前端组件卸载来回收进程。

## 推荐决策与实施顺序

### 现在（不引入 LangChain 依赖）

- 保留现有三块工作区和新终端工作台的 UI/PTY 架构；
- 把监督闭环抽象成事件化 `SupervisorEngine`，但继续使用现有 Rust/PowerShell 实现；
- 为每个任务记录稳定的 task ID、状态转移、轮次和产物引用；
- 让 UI 只依赖结构化事件，不依赖具体 PowerShell 输出文本；
- 保留现有会话 JSONL/MCP 为浏览事实来源。

### 未来触发条件（满足其一再评估 LangGraph）

- 固定 L0/L1/L2 不能表达需要的动态流程；
- 需要跨重启可靠恢复或远程/长时间运行；
- 需要多 Agent 子图、人工审批和可观测 trace；
- 监督逻辑在 Rust/PowerShell 中出现难以测试的分支爆炸。

### 采用时的最小切面

1. 先用 LangGraph.js 做纯内存 mock graph，验证状态、事件和取消契约；
2. 再接本地 IPC 的只读工具（读取审查产物、查询状态）；
3. 最后接“启动/恢复/取消 CLI”这类有副作用工具，并在每个节点上实现幂等和审批；
4. 通过 feature flag 与现有 engine 并行跑回归，不直接替换生产路径；
5. 对 sidecar、依赖许可证、网络访问、敏感字段和崩溃恢复做桌面打包测试。

## 检索限制

本次执行环境无法建立外部 HTTPS 连接（PowerShell/curl 报告 Windows Schannel `SEC_E_NO_CREDENTIALS`，升级网络权限请求也未能建立可用通道），因此没有把网络抓取结果冒充为本地验证过的当前版本源码。文中只引用 LangChain/LangGraph 官方文档与官方仓库的规范 URL；正式发布前请在目标版本上重新核对 API、LICENSE 和传递依赖。

