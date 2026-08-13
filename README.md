# Harness Agent

驾驭 Claude 干活 + Codex 审查的桌面端监督闭环应用。

## 定位

- **便宜模型**（Claude/DeepSeek/Kimi/GLM）干活
- **贵模型**（Codex）读 JSONL 审查 + 意见回灌返工
- **会话可见性**：复用 mcp-lab 的 agent-sessions-mcp

## 技术栈

- Tauri 2（Rust，GNU 工具链 `x86_64-pc-windows-gnu`）
- React + TypeScript + Vite

## 开发状态

- [ ] 阶段 0：选型验证 + 脚手架（进行中）
- [ ] 阶段 1：会话浏览器
- [ ] 阶段 2：闭环接入 + 审查看板
- [ ] 阶段 3：MCP 健康检查 + 打包

## 目录约定

- `src/`：React 前端
- `src-tauri/`：Rust 后端（薄 command 层，零业务逻辑）
- Rust 工具链位于 `F:\develop_soft\IDE\AI_tools\Rust_env`（不占 C 盘）
