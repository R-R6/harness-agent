//! session_proxy —— 内置的 agent-sessions-mcp (server.js) 会话数据源
//!
//! server.js 随应用打包（src-tauri/resources/agent-sessions-mcp/server.js），
//! 本 crate 只做 stdio 转发，不实现会话解析逻辑。
//!
//! 设计约束（计划 v3，审阅者硬性要求）：
//! - 本 crate **永不直读会话 JSONL**，会话数据一律经 server.js 提供
//!   （server.js 内置路径白名单 isWithinSessionsRoot，直读会绕过它）
//! - 每次调用 spawn node server.js（stdio JSON-RPC），简单可靠，无长驻进程生命周期问题
//! - server.js 支持 AGENT_SESSIONS_CLAUDE_ROOT / AGENT_SESSIONS_CODEX_ROOT 环境变量
//!   覆盖真实会话根目录 —— 测试时指向伪造目录，不影响真实环境
//!
//! 独立 crate 的原因：tauri 依赖链（webview2-com-sys）在 GNU 目标下会引入
//! WebView2Loader.dll 运行时依赖，导致 cargo test 崩溃（STATUS_ENTRYPOINT_NOT_FOUND）。
//! 拆成无 tauri 依赖的独立 crate 后，`cargo test -p session_proxy` 可正常跑。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ---------------- 数据结构（与 server.js 返回契约对齐） ----------------

/// 会话列表项（对应 server.js list_sessions 返回的每行）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub agent: String,
    #[serde(rename = "agentLabel")]
    pub agent_label: String,
    pub file: String,
    /// 会话标题（ai-title/custom-title/session_meta.title），空则前端回退文件名
    #[serde(default)]
    pub title: String,
    /// 会话的原始工作目录（Claude JSONL 每行记录，续聊时还原启动目录；
    /// Codex rollout 正文无 cwd 则为空）
    #[serde(default)]
    pub cwd: String,
    pub updated: String,
}

/// transcript 条目（对应 server.js get_transcript 返回的每项）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub text: String,
    #[serde(default)]
    pub at: Option<String>,
}

// ---------------- 进程/路径定位 ----------------

/// node 可执行文件：优先环境变量 HARNESS_NODE_PATH，否则走 PATH 的 node
fn node_path() -> String {
    std::env::var("HARNESS_NODE_PATH").unwrap_or_else(|_| "node".to_string())
}

/// server.js 路径（三级解析）：
/// 1. 环境变量 HARNESS_MCP_SERVER（测试/调试覆盖）
/// 2. tauri setup 注入的打包资源路径（发布/开发，来自 BaseDirectory::Resource）
/// 3. 兜底：仓库内副本（cargo test 无注入时的默认值）
static SERVER_JS: OnceLock<PathBuf> = OnceLock::new();

/// 由 tauri 启动时注入打包资源目录里的真实 server.js 路径
pub fn set_server_js(path: PathBuf) {
    let _ = SERVER_JS.set(path);
}

fn server_js_path() -> PathBuf {
    path_util::resolve_script(
        "HARNESS_MCP_SERVER",
        SERVER_JS.get(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/agent-sessions-mcp/server.js"
        ),
    )
}

// ---------------- MCP stdio 客户端 ----------------

/// 向 server.js 发一次 tools/call，返回解析后的结果 JSON
fn call_tool(name: &str, args: &Value) -> Result<Value, String> {
    let mut command = Command::new(node_path());
    // 发布版是 GUI 子系统：不加 CREATE_NO_WINDOW 每次 spawn node 都会闪控制台
    path_util::no_console_window(&mut command);
    let mut child = command
        .arg(server_js_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()) // 诊断信息透传，便于排查
        .spawn()
        .map_err(|e| format!("spawn node 失败（请确认 Node.js 可用）: {e}"))?;

    let mut stdin = child.stdin.take().ok_or("无法获取 stdin")?;

    // initialize 握手（MCP 必需）
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "harness-agent", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    writeln!(stdin, "{init}").map_err(|e| format!("写入 initialize 失败: {e}"))?;

    // tools/call
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    writeln!(stdin, "{call}").map_err(|e| format!("写入 tools/call 失败: {e}"))?;

    // 关闭 stdin → server.js 处理完所有行后自然退出
    drop(stdin);

    let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
    let reader = BufReader::new(stdout);

    for line in reader.lines() {
        let line = line.map_err(|e| format!("读取响应失败: {e}"))?;
        let v: Value = serde_json::from_str(&line).map_err(|e| format!("响应不是合法 JSON: {e}"))?;
        // 只取 id=2（tools/call）的响应；id=1 是 initialize 响应，跳过
        if v.get("id").and_then(Value::as_u64) != Some(2) {
            continue;
        }
        // 工具错误（server.js 把异常包成 isError 响应）
        if v["result"]["isError"].as_bool() == Some(true) {
            let msg = v["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("未知工具错误");
            return Err(msg.to_string());
        }
        let text = v["result"]["content"][0]["text"]
            .as_str()
            .ok_or("响应缺少 content.text")?;
        return serde_json::from_str(text).map_err(|e| format!("解析工具结果失败: {e}"));
    }

    Err("server.js 未返回响应".to_string())
}

// ---------------- 公开工具函数（供 tauri commands 调用） ----------------

/// 列出会话（agent 可选：claude / codex；limit 可选：每 agent 条数，默认 20 上限 200）
pub fn list_sessions(agent: Option<String>, limit: Option<usize>) -> Result<Vec<SessionInfo>, String> {
    let mut args = serde_json::Map::new();
    if let Some(a) = agent {
        args.insert("agent".into(), json!(a));
    }
    if let Some(l) = limit {
        args.insert("limit".into(), json!(l));
    }
    let body = call_tool("list_sessions", &json!(args))?;
    serde_json::from_value(body).map_err(|e| format!("解析 list_sessions 结果失败: {e}"))
}

/// 读取某会话正文（tail：只取末尾 N 条，默认 200；offset：从末尾跳过 N 条，配合 tail 往前翻页到会话开头）
pub fn get_transcript(file: &str, tail: Option<i64>, offset: Option<i64>) -> Result<Vec<TranscriptEntry>, String> {
    let mut args = json!({ "file": file });
    if let Some(t) = tail {
        args["tail"] = json!(t);
    }
    if let Some(o) = offset {
        args["offset"] = json!(o);
    }
    let v = call_tool("get_transcript", &args)?;
    serde_json::from_value(v).map_err(|e| format!("解析 get_transcript 结果失败: {e}"))
}

/// 按关键词全文搜索会话
pub fn search_sessions(keyword: &str) -> Result<Vec<SessionInfo>, String> {
    let v = call_tool("search_sessions", &json!({ "keyword": keyword }))?;
    serde_json::from_value(v).map_err(|e| format!("解析 search_sessions 结果失败: {e}"))
}

// ---------------- 测试 ----------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量是进程级全局，测试并发会互相覆盖 AGENT_SESSIONS_*_ROOT，
    /// 必须串行化（cargo test 默认多线程）
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 构造一个最小可用的伪造会话目录结构：
    /// {tmp}/fake_claude/<slug>/<uuid>.jsonl + {tmp}/fake_codex/2026/08/13/rollout-*.jsonl
    fn build_fake_sessions(tmp: &std::path::Path) -> (String, String) {
        let claude_root = tmp.join("fake_claude");
        let codex_root = tmp.join("fake_codex");
        let claude_dir = claude_root.join("my-project");
        let codex_dir = codex_root.join("2026").join("08").join("13");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::create_dir_all(&codex_dir).unwrap();

        // Claude 会话：user + assistant + ai-title（cwd 记录在每行 JSONL）
        let claude_file = claude_dir.join("session-abc.jsonl");
        std::fs::write(
            &claude_file,
            "{\"type\":\"ai-title\",\"aiTitle\":\"写计算器\",\"cwd\":\"F:\\\\work\\\\proj\"}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"写个计算器\"},\"cwd\":\"F:\\\\work\\\\proj\"}\n\
             {\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"好的，我写一个\"}]}}\n",
        )
        .unwrap();

        // Codex 会话
        let codex_file = codex_dir.join("rollout-test.jsonl");
        std::fs::write(
            &codex_file,
            "{\"type\":\"session_meta\",\"model\":\"gpt-5.6-luna\"}\n\
             {\"type\":\"response_item\",\"payload\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"猜数字游戏\"}]}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"已完成猜数字\"}}\n",
        )
        .unwrap();

        (
            claude_root.to_string_lossy().to_string(),
            codex_root.to_string_lossy().to_string(),
        )
    }

    /// 在测试中把环境变量指到伪造目录，跑真实 server.js 链路
    fn with_fake_sessions<T>(f: impl FnOnce() -> T) -> T {
        let _guard = SERIAL.lock().unwrap(); // 串行化：环境变量是进程级全局
        let tmp = std::env::temp_dir().join(format!(
            "harness-session-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let (claude_root, codex_root) = build_fake_sessions(&tmp);
        // 必须还原旧值，测试间互不污染
        let old_claude = std::env::var("AGENT_SESSIONS_CLAUDE_ROOT").ok();
        let old_codex = std::env::var("AGENT_SESSIONS_CODEX_ROOT").ok();
        std::env::set_var("AGENT_SESSIONS_CLAUDE_ROOT", &claude_root);
        std::env::set_var("AGENT_SESSIONS_CODEX_ROOT", &codex_root);
        let r = f();
        match old_claude {
            Some(v) => std::env::set_var("AGENT_SESSIONS_CLAUDE_ROOT", v),
            None => std::env::remove_var("AGENT_SESSIONS_CLAUDE_ROOT"),
        }
        match old_codex {
            Some(v) => std::env::set_var("AGENT_SESSIONS_CODEX_ROOT", v),
            None => std::env::remove_var("AGENT_SESSIONS_CODEX_ROOT"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
        r
    }

    #[test]
    fn list_sessions_returns_both_agents() {
        with_fake_sessions(|| {
            let rows = list_sessions(None, None).expect("list_sessions 应成功");
            let agents: Vec<&str> = rows.iter().map(|r| r.agent.as_str()).collect();
            assert!(agents.contains(&"claude"), "应包含 claude: {rows:?}");
            assert!(agents.contains(&"codex"), "应包含 codex: {rows:?}");
            assert!(rows.iter().any(|r| r.file.ends_with("session-abc.jsonl")));
            assert!(rows.iter().any(|r| r.file.ends_with("rollout-test.jsonl")));
        });
    }

    /// Codex 标题不在 rollout 正文里，由 server.js 从 CODEX_ROOT 父目录的
    /// session_index.jsonl 反查（文件名 UUID ↔ 索引 id ↔ thread_name）
    #[test]
    fn list_sessions_uses_codex_index_title() {
        with_fake_sessions(|| {
            let codex_root =
                std::env::var("AGENT_SESSIONS_CODEX_ROOT").expect("闭包内 env 已设置");
            // 索引文件在 CODEX_ROOT 的父目录（模拟 ~/.codex/session_index.jsonl 布局）
            let index = std::path::Path::new(&codex_root)
                .parent()
                .unwrap()
                .join("session_index.jsonl");
            std::fs::write(
                &index,
                concat!(
                    "{\"id\":\"0199abcd-0000-7000-8000-00000000abcd\",\"thread_name\":\"索引旧名\"}\n",
                    "坏行不是 JSON\n",
                    "{\"id\":\"0199abcd-0000-7000-8000-00000000abcd\",\"thread_name\":\"索引标题-重命名后\"}\n",
                ),
            )
            .unwrap();
            let rollout = std::path::Path::new(&codex_root)
                .join("2026")
                .join("08")
                .join("13")
                .join("rollout-2026-08-13T00-00-00-0199abcd-0000-7000-8000-00000000abcd.jsonl");
            std::fs::write(
                &rollout,
                "{\"type\":\"session_meta\",\"payload\":{}}\n",
            )
            .unwrap();

            let rows = list_sessions(Some("codex".into()), Some(50)).expect("应成功");
            let uuid_row = rows
                .iter()
                .find(|r| r.file.contains("0199abcd"))
                .expect("找到 UUID 命名的 codex 会话");
            assert_eq!(
                uuid_row.title, "索引标题-重命名后",
                "标题应取索引里最新 thread_name"
            );
            let plain = rows
                .iter()
                .find(|r| r.file.ends_with("rollout-test.jsonl"))
                .expect("找到无 UUID 的 codex 会话");
            assert_eq!(plain.title, "", "无 UUID 文件不查索引，回退空标题");
        });
    }

    #[test]
    fn list_sessions_filters_by_agent() {
        with_fake_sessions(|| {
            let rows = list_sessions(Some("claude".into()), None).expect("应成功");
            assert!(!rows.is_empty());
            assert!(rows.iter().all(|r| r.agent == "claude"));
        });
    }

    /// Claude 会话从 JSONL 头部透出原始工作目录（slug 有损不可反解），
    /// Codex rollout 正文无 cwd 则为空
    #[test]
    fn list_sessions_returns_claude_cwd() {
        with_fake_sessions(|| {
            let rows = list_sessions(Some("claude".into()), None).unwrap();
            let row = rows
                .iter()
                .find(|r| r.file.ends_with("session-abc.jsonl"))
                .expect("找到 claude 会话");
            assert_eq!(row.cwd, "F:\\work\\proj", "应透出 JSONL 记录的原始 cwd");
            let codex = list_sessions(Some("codex".into()), None).unwrap();
            assert!(codex.iter().all(|r| r.cwd.is_empty()));
        });
    }

    #[test]
    fn get_transcript_parses_both_formats() {
        with_fake_sessions(|| {
            let rows = list_sessions(None, None).unwrap();
            let claude = rows
                .iter()
                .find(|r| r.file.ends_with("session-abc.jsonl"))
                .expect("找到 claude 会话");
            let t = get_transcript(&claude.file, None, None).expect("transcript 应成功");
            assert!(t.iter().any(|e| e.text.contains("写个计算器")), "含 user 消息");
            assert!(t.iter().any(|e| e.text.contains("好的，我写一个")), "含 assistant 消息");
            assert!(t.iter().any(|e| e.text.contains("写计算器")), "含标题");

            let codex = rows
                .iter()
                .find(|r| r.file.ends_with("rollout-test.jsonl"))
                .expect("找到 codex 会话");
            let t2 = get_transcript(&codex.file, None, None).expect("codex transcript 应成功");
            assert!(t2.iter().any(|e| e.text.contains("猜数字游戏")), "含 codex user 消息");
            assert!(t2.iter().any(|e| e.text.contains("已完成猜数字")), "含 codex agent 消息");
        });
    }

    #[test]
    fn get_transcript_respects_tail() {
        with_fake_sessions(|| {
            let rows = list_sessions(Some("claude".into()), None).unwrap();
            let t_all = get_transcript(&rows[0].file, None, None).unwrap();
            let t_1 = get_transcript(&rows[0].file, Some(1), None).unwrap();
            assert!(t_1.len() <= 1, "tail=1 应只返回 1 条，实际 {}", t_1.len());
            assert!(t_all.len() >= 3, "完整应至少 3 条，实际 {}", t_all.len());
        });
    }

    #[test]
    fn get_transcript_respects_offset() {
        with_fake_sessions(|| {
            let rows = list_sessions(Some("claude".into()), None).unwrap();
            let file = &rows[0].file;
            // 伪造 claude 会话 3 条：title（写计算器）、user、assistant
            let t_all = get_transcript(file, None, None).unwrap();
            assert_eq!(t_all.len(), 3, "伪造 claude 会话应 3 条，实际 {}", t_all.len());
            // offset=0：与无 offset 一致（向后兼容）
            let t_last = get_transcript(file, Some(2), None).unwrap();
            assert_eq!(t_last.len(), 2);
            // offset=2：跳过末尾 2 条，取更早的 1 条（与末尾不重叠）
            let t_prev = get_transcript(file, Some(2), Some(2)).unwrap();
            assert_eq!(t_prev.len(), 1, "offset 后应只剩最早 1 条，实际 {}", t_prev.len());
            assert!(t_prev[0].text.contains("写计算器"), "最早那条是标题，实际 {}", t_prev[0].text);
            assert_ne!(t_prev[0].text, t_last[0].text, "offset 后应取更早消息");
            // offset 超总量 → 空（已到最前）
            assert!(get_transcript(file, Some(2), Some(999)).unwrap().is_empty());
        });
    }

    #[test]
    fn search_sessions_finds_keyword() {
        with_fake_sessions(|| {
            let hits = search_sessions("计算器").expect("搜索应成功");
            assert!(hits.iter().any(|r| r.file.ends_with("session-abc.jsonl")));
            let hits2 = search_sessions("不存在的词xyz").unwrap();
            assert!(hits2.is_empty());
        });
    }

    #[test]
    fn get_transcript_rejects_path_outside_whitelist() {
        // 白名单外文件（即使存在）必须被 server.js 拒绝 —— 这是 S1 安全边界的回归测试
        with_fake_sessions(|| {
            let evil = "C:\\Windows\\system32\\drivers\\etc\\hosts";
            let err = get_transcript(evil, None, None).expect_err("白名单外必须拒绝");
            assert!(err.contains("拒绝") || err.contains("不在会话根目录"), "错误信息: {err}");
        });
    }

    #[test]
    fn get_transcript_rejects_nonexistent_file() {
        with_fake_sessions(|| {
            let rows = list_sessions(Some("claude".into()), None).unwrap();
            let fake = format!("{}__nope.jsonl", rows[0].file);
            let err = get_transcript(&fake, None, None).expect_err("不存在必须报错");
            assert!(err.contains("不存在"), "错误信息: {err}");
        });
    }
}
