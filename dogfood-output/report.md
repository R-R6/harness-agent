# Dogfood QA Report — Harness Agent

**Target:** Harness Agent 桌面应用（Tauri 2 + React + Rust，dev 端口 127.0.0.1:14200）
**Date:** 2026-08-14
**Scope:** 全功能代码审查（前端 React + Rust 后端 + MCP 桥），依赖安全，运行环境
**Tester:** Hermes Agent（静态审查 + 依赖扫描 + 运行验证，本环境无浏览器工具，采用等价方法）

---

## Executive Summary

| Severity | Count |
|----------|-------|
| 🔴 Critical | 0 |
| 🟠 High | 2 |
| 🟡 Medium | 4 |
| 🔵 Low | 6 |
| **Total** | **12** |

**Overall Assessment:** 核心功能（会话浏览/监督闭环/MCP 自检）实现扎实、测试覆盖好（35 前端 + 21 Rust），无致命缺陷；主要问题集中在**进程生命周期管理**（取消不杀子进程）和**安全纵深防御**（无 CSP）两项 High 级，以及若干输入校验/边界处理。

---

## Issues

### Issue #1: 取消监督任务只杀 pwsh 主进程，claude/codex 子进程残留

| Field | Value |
|-------|-------|
| **Severity** | 🟠 High |
| **Category** | Functional |
| **URL** | 监督闭环 → 取消 |

**Description:**
`cancel_supervise`（lib.rs:111-120）调用 `child.kill()` 只终止 pwsh 主进程，而 pwsh 派生的子进程（claude -p / codex exec / node server.js）不会被级联终止。取消任务后这些子进程可能继续运行数分钟，继续调用 API、写文件、占资源。

**Steps to Reproduce:**
1. 启动真实模式监督任务（claude 干活中）
2. 点"取消"
3. 查进程：claude/codex/node 进程仍存活（之前实测：codex 10628 任务结束后仍在跑）

**Expected Behavior:** 取消时级联终止整个进程树（taskkill /T 或 Job Object）

**Actual Behavior:** 仅 pwsh 退出，子进程继续运行

---

### Issue #2: Content-Security-Policy 未配置（csp: null）

| Field | Value |
|-------|-------|
| **Severity** | 🟠 High |
| **Category** | Console / Security |
| **URL** | 全局 |

**Description:**
tauri.conf.json `security.csp` 为 `null`（无 CSP）。Tauri 2 官方强烈建议配置 CSP（`default-src 'self'` + `connect-src ipc: http://ipc.localhost` 等）。虽然当前前端用 React 文本节点渲染会话内容（`<pre>{e.text}</pre>`，自动转义，无 XSS 面），但无 CSP 意味着未来任何 `dangerouslySetInnerHTML` 或第三方资源引入都会零防御。

**Steps to Reproduce:**
1. 打开 tauri.conf.json 查看 `"security": { "csp": null }`
2. Tauri 启动日志有 CSP 缺失警告

**Expected Behavior:** 配置最小化 CSP（`"csp": "default-src 'self'; connect-src ipc: http://ipc.localhost"`）

**Actual Behavior:** 无 CSP，任何内联/远程脚本理论上可执行

---

### Issue #3: 快捷键 Ctrl+1/2/3 在输入框聚焦时误切视图

| Field | Value |
|-------|-------|
| **Severity** | 🟡 Medium |
| **Category** | Functional |
| **URL** | 会话浏览搜索框 / 监督闭环表单 |

**Description:**
App.tsx 的全局 keydown 监听（line 94-100）未检查事件目标。在搜索框、任务描述、工作目录输入框聚焦时按 Ctrl+1/2/3 会意外切换 tab，中断输入流。例如用户在监督闭环表单输入时按 Ctrl+2 想全选文本（某些输入法习惯），实际切走了页面。

**Steps to Reproduce:**
1. 聚焦搜索框输入文字
2. 按 Ctrl+1
3. 视图切到会话浏览（预期：无操作）

**Expected Behavior:** 输入元素（INPUT/TEXTAREA/SELECT）聚焦时快捷键不生效

**Actual Behavior:** 无条件切换 tab

---

### Issue #4: 监督任务失败无退出码信息，无法区分成功/失败

| Field | Value |
|-------|-------|
| **Severity** | 🟡 Medium |
| **Category** | UX |
| **URL** | 监督闭环 |

**Description:**
`supervise-done` 事件（lib.rs:103）只携带 taskId，不携带退出码/错误。supervise.ps1 失败（exit 1，如 claude 不可用、上游 429）时，前端日志流结束但没有失败标识，用户难以判断任务是"验收通过"还是"运行失败"——需要自己翻日志找报错。

**Steps to Reproduce:**
1. 让 supervise.ps1 失败（如临时改坏脚本路径）
2. 启动任务 → 日志流很快结束
3. 前端无任何失败提示

**Expected Behavior:** done 事件携带 exit code + 失败时红色提示

**Actual Behavior:** 静默结束，无成败区分

---

### Issue #5: workDir 未校验存在性/非空

| Field | Value |
|-------|-------|
| **Severity** | 🟡 Medium |
| **Category** | Functional |
| **URL** | 监督闭环 → 启动 |

**Description:**
SupervisePanel.start()（line 50-68）只校验 task 非空，workDir 为空或指向不存在的目录时直接提交，spawn 失败后才在 error 区显示错误。用户填错路径时反馈滞后且不明确（pwsh 报错夹杂英文路径信息）。

**Steps to Reproduce:**
1. 清空工作目录输入框
2. 填任务 → 点启动
3. 报错信息不友好

**Expected Behavior:** 启动前校验目录存在，给出明确中文提示

**Actual Behavior:** 提交后由 pwsh 报错

---

### Issue #6: 大会话每次点击全量读取+解析（无缓存）

| Field | Value |
|-------|-------|
| **Severity** | 🟡 Medium |
| **Category** | Performance |
| **URL** | 会话浏览 → 点选会话 |

**Description:**
getTranscript 每次调用都 `readFileSync` 整个文件（上限 8MB）再逐行 JSON.parse。频繁切换会话时重复读盘+解析；超长会话（几千行）点击有明显延迟。server.js 虽有 MAX_LINES 截断但发生在全量读取之后。

**Steps to Reproduce:**
1. 打开一个大会话（>2MB）
2. 快速来回切换几个会话
3. 正文区出现明显加载延迟

**Expected Behavior:** 缓存已读会话或服务端流式截断

**Actual Behavior:** 每次全量读+解析

---

### Issue #7: export_transcript_md 的 dest 无路径约束

| Field | Value |
|-------|-------|
| **Severity** | 🔵 Low |
| **Category** | Security |
| **URL** | 会话右键 → 导出 |

**Description:**
lib.rs:142-156 的 `export_transcript_md` 直接 `fs::write(&dest, md)`，无任何路径校验。前端通过 save 对话框限制用户选择，但 command 本身可被任意调用写入任意路径（本地单用户应用，风险有限，属于纵深防御缺口）。

**Expected Behavior:** 校验 dest 为绝对路径且是 .md 后缀

**Actual Behavior:** 任意路径可写

---

### Issue #8: task_id 用毫秒时间戳，同毫秒启动可能碰撞

| Field | Value |
|-------|-------|
| **Severity** | 🔵 Low |
| **Category** | Functional |
| **URL** | 监督闭环 |

**Description:**
lib.rs:63-69 task_id = `task-{unix_millis}`。理论上同一毫秒启动两个任务（不同 workDir）时 id 相同，后启动的覆盖 running map 中前一个，前一个任务失去取消能力。

**Expected Behavior:** 使用自增计数器或 UUID

**Actual Behavior:** 毫秒时间戳，碰撞时覆盖

---

### Issue #9: toast 定时器组件卸载时未清理

| Field | Value |
|-------|-------|
| **Severity** | 🔵 Low |
| **Category** | Console |
| **URL** | 会话浏览 |

**Description:**
SessionList 的 toastTimer（useRef 存 timeout id）在组件卸载时未 clearTimeout。虽然 SessionList 常驻（双视图 CSS 显隐不卸载），但严格模式/测试环境会泄漏计时器。

**Expected Behavior:** useEffect cleanup 清理 timer

**Actual Behavior:** 无清理

---

### Issue #10: 右键菜单位置在窗口 resize 后不出界纠正

| Field | Value |
|-------|-------|
| **Severity** | 🔵 Low |
| **Category** | Visual |
| **URL** | 会话浏览右键菜单 |

**Description:**
菜单打开时用 `Math.min(clientX, innerWidth-220)` 防出界，但窗口 resize 后 fixed 定位的菜单不会重新计算位置，可能被裁切。

**Expected Behavior:** resize 时关闭或重定位菜单

**Actual Behavior:** 位置保持，可能出界

---

### Issue #11: 长消息截断（2000 字符）无提示

| Field | Value |
|-------|-------|
| **Severity** | 🔵 Low |
| **Category** | Content |
| **URL** | 会话正文 |

**Description:**
server.js MAX_LINE_CHARS=2000 截断超长消息，但正文渲染无"…已截断"提示，用户不知道内容被截。

**Expected Behavior:** 截断处显示省略标记

**Actual Behavior:** 静默截断

---

### Issue #12: 并发锁 normalize_path 不解析 `..`，同目录不同写法可绕过

| Field | Value |
|-------|-------|
| **Severity** | 🔵 Low |
| **Category** | Functional |
| **URL** | 监督闭环 |

**Description:**
normalize_path 只去尾部斜杠+小写，不 resolve `..`/`.`。`F:\a\b` 与 `F:\a\b\..\b` 被视为不同目录，可绕过 busy_workdirs 并发锁同时跑两个任务。

**Expected Behavior:** 用 std::path 规范化后比较

**Actual Behavior:** 仅字符串归一化

---

## Issues Summary Table

| # | Title | Severity | Category |
|---|-------|----------|----------|
| 1 | 取消不杀子进程（孤儿残留） | 🟠 High | Functional |
| 2 | CSP 未配置 | 🟠 High | Security |
| 3 | 快捷键输入框误切 tab | 🟡 Medium | Functional |
| 4 | 任务失败无退出码提示 | 🟡 Medium | UX |
| 5 | workDir 未校验 | 🟡 Medium | Functional |
| 6 | 大会话无缓存 | 🟡 Medium | Performance |
| 7 | 导出路径无约束 | 🔵 Low | Security |
| 8 | task_id 毫秒碰撞 | 🔵 Low | Functional |
| 9 | toast 定时器泄漏 | 🔵 Low | Console |
| 10 | 菜单 resize 不出界 | 🔵 Low | Visual |
| 11 | 长消息截断无提示 | 🔵 Low | Content |
| 12 | 并发锁可绕过 | 🔵 Low | Functional |

## Testing Coverage

### 已验证（通过）
- npm audit：0 漏洞
- 前端测试 35/35、Rust 测试 21/21（session_proxy 7 + supervise_runner 5 + mcp_checker 7 + 基线）
- dev 运行无报错；server.js 白名单（isWithinSessionsRoot 防路径穿越，normalize+startsWith 有效）
- spawn 参数用 argv 传递（无 shell 注入面）；tail/limit 参数有 clamp（负数/0 安全）
- 右键菜单已修（stopPropagation）；分割线拖动正常；MCP 自检/自修复逻辑测试覆盖

### 未覆盖 / 无法测试
- 真实 GUI 交互（本环境无浏览器工具）——需用户在窗口中复现 #3/#5/#6
- 极端网络/上游故障场景（HUBWAY 429 等）

---

## Notes

1. **#1 是最高优先修复项**：可加 Rust 进程树终止（Windows 上 spawn `taskkill /PID <pid> /T /F`，或建立 Job Object）。之前会话里已观察到取消后 codex 进程残留的真实案例。
2. **#2 CSP 配置**：一行 JSON 改动，建议立即做（`"csp": "default-src 'self'; connect-src ipc: http://ipc.localhost"`）。
3. 所有 High/Medium 项均为小改动，可在一轮内修完。
