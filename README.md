# Harness Agent

驾驭 Claude 干活 + Codex 审查的桌面端监督闭环应用。

## 定位

- **便宜模型**（Claude/DeepSeek/Kimi/GLM）干活
- **贵模型**（Codex）读会话审查 + 意见回灌返工
- **两种监督形态**：无头闭环（表单派活，supervise.ps1）/ **终端驱动**（任务注入可见
  Claude 终端 pane，干活全程可见、人可随时插手——supervise_engine）
- **会话可见性**：内置 agent-sessions-mcp（会话数据读取，随应用打包）

## 技术栈

- Tauri 2（Rust，GNU 工具链 `x86_64-pc-windows-gnu`）
- React + TypeScript + Vite

## 开发状态

- [x] 阶段 0：选型验证 + 脚手架
- [x] 阶段 1：会话浏览器（双栏列表/正文/搜索/续聊/导出）
- [x] 阶段 2：闭环接入 + 审查看板（无头 + 终端驱动双模式）
- [x] 阶段 3：MCP 健康检查 + 打包
- [ ] 阶段 4：macOS 支持

## 目录结构

```
Harness_agent/
├── src/                          # React 前端
├── src-tauri/
│   ├── src/lib.rs                # Tauri command 装配层
│   ├── crates/
│   │   ├── terminal_host/        # ConPTY + CLI 启动
│   │   ├── session_proxy/        # agent-sessions-mcp 桥
│   │   ├── supervise_runner/     # 无头闭环进程桥（supervise.ps1）
│   │   ├── supervise_engine/     # 终端驱动监督引擎（阶段 2）
│   │   ├── mcp_checker/          # MCP 注册健康检查/修复
│   │   └── path_util/            # 共享路径/进程工具
│   └── resources/                # 随包分发的 server.js / supervise.ps1
└── docs/                         # 文档（按类归档）
    ├── 使用指南/                 # 安装与使用指南
    ├── 计划/                     # 工作空间与多任务管理改造计划
    ├── 交接文档/                 # 工作空间改造接续指南、现状与 Codex 差距
    ├── 研究调研/                 # LangChain 评估、阶段 1.5 spike 结论
    └── 设计规格/                 # 工作台 UI / 布局设计规格
```

## 开发命令

```bash
npm install          # 装前端依赖
npm run tauri dev    # 开发模式（需 tauri-cli）
npm run tauri build  # 构建发布版
npm test             # 前端测试（vitest）
cargo test           # Rust 测试（src-tauri 下，各 crate 独立可跑）
```

## 目录约定

- Rust 工具链位于 `F:\develop_soft\IDE\AI_tools\Rust_env`（不占 C 盘）
- 首版范围：Windows 内部版（macOS 列阶段 4）
