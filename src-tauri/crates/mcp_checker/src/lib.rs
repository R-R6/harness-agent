//! mcp_checker —— Codex agent-sessions MCP 注册健康检查
//!
//! 背景：Codex 桌面端 / codex++ 启动器会整体重写 ~/.codex/config.toml，
//! 手工加的 [mcp_servers.agent-sessions] 条目会被冲掉（已知坑 codex++ #353）。
//! 本 crate 提供：
//! 1. `check_mcp`：用 toml crate **结构化解析** config.toml（比正则可靠），
//!    检查注册/type/command/args/server.js/宿主 exe，并做真实 MCP 握手
//! 2. `fix_mcp`：**最小侵入修复**（备份 + 行级插入缺失项 + 原子写），
//!    保留用户 config.toml 里所有其他内容与注释（不用 toml::Value 往返，
//!    因为序列化会丢注释/重排整个文件，破坏用户配置）
//!
//! 注意：本 crate 检查的是 **Codex 生态**在 ~/.codex/config.toml 里注册的
//! agent-sessions MCP：检查以 config 实际注册内容为准；fix_mcp 写入的 server.js
//! 默认取 Harness 内置副本（resources/agent-sessions-mcp/server.js，三级解析），
//! 在只安装了 Harness、没有 mcp-lab 仓库的机器上也能完成修复。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

// ---------------- 数据结构（契约） ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckItem {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStatus {
    pub config_path: String,
    pub items: Vec<CheckItem>,
    pub server_js_exists: bool,
    pub host_exe_exists: bool,
    pub handshake_ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixResult {
    pub backup_path: Option<String>,
    pub fixed_items: Vec<String>,
    pub ok: bool,
    pub message: String,
}

// ---------------- 路径定位 ----------------

/// ~/.codex/config.toml 默认路径（跟随当前用户 home，不硬编码任何机器）
pub fn default_config_path() -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    Path::new(&home)
        .join(".codex")
        .join("config.toml")
        .to_string_lossy()
        .to_string()
}

/// 修复时写入 args 的 server.js 路径（三级解析，与 session_proxy 同规则）：
/// 1. 环境变量 HARNESS_MCP_SERVER（测试/调试覆盖）
/// 2. tauri setup 注入的打包资源路径（发布/开发）
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

/// codex-code-mode-host.exe 定位：环境变量 HARNESS_CODEX_HOST_EXE 覆盖 →
/// 任一 codex CLI（where/which 可能返回多个：npm shim 与真实安装）所在目录的同名文件
/// （Codex ≥0.147 需要）
fn locate_host_exe() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("HARNESS_CODEX_HOST_EXE") {
        let p = PathBuf::from(v);
        return p.exists().then_some(p);
    }
    let which = if cfg!(windows) { "where" } else { "which" };
    let out = Command::new(which).arg("codex").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|codex| {
            let exe = Path::new(codex).parent()?.join("codex-code-mode-host.exe");
            exe.exists().then_some(exe)
        })
        .next()
}

// ---------------- 检查 ----------------

/// 完整健康检查：5 项注册检查 + 文件存在性 + 真实握手
pub fn check_mcp(config_path: &str) -> McpStatus {
    let mut items = vec![];

    // 1. config.toml 存在
    let cfg_exists = Path::new(config_path).exists();
    items.push(CheckItem {
        name: "config.toml 存在".into(),
        ok: cfg_exists,
        detail: if cfg_exists {
            config_path.to_string()
        } else {
            format!("{config_path} 不存在（Codex CLI 安装异常？）")
        },
    });

    // 2-5. 结构化解析注册状态
    let mut reg_ok = false;
    let mut type_ok = false;
    let mut cmd_ok = false;
    let mut args_ok = false;
    // config 实际注册的 server.js 路径（第 6 项检查与握手都以它为准）
    let mut registered_server: Option<String> = None;
    if cfg_exists {
        match std::fs::read_to_string(config_path) {
            Ok(raw) => match raw.parse::<toml::Value>() {
                Ok(v) => {
                    let section = v
                        .get("mcp_servers")
                        .and_then(|m| m.get("agent-sessions"));
                    reg_ok = section.is_some();
                    type_ok = section
                        .and_then(|s| s.get("type"))
                        .and_then(|t| t.as_str())
                        == Some("stdio");
                    cmd_ok = section.and_then(|s| s.get("command")).is_some();
                    args_ok = section
                        .and_then(|s| s.get("args"))
                        .and_then(|a| a.as_array())
                        .map(|arr| {
                            arr.iter().any(|x| {
                                x.as_str()
                                    .map(|s| s.contains("server.js"))
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false);
                    registered_server = section
                        .and_then(|s| s.get("args"))
                        .and_then(|a| a.as_array())
                        .and_then(|arr| {
                            arr.iter().find_map(|x| {
                                x.as_str()
                                    .filter(|s| s.contains("server.js"))
                                    .map(str::to_string)
                            })
                        });
                }
                Err(e) => items.push(CheckItem {
                    name: "config.toml 可解析".into(),
                    ok: false,
                    detail: format!("TOML 解析失败: {e}"),
                }),
            },
            Err(e) => items.push(CheckItem {
                name: "config.toml 可读".into(),
                ok: false,
                detail: format!("读取失败: {e}"),
            }),
        }
    }

    items.push(CheckItem {
        name: "[mcp_servers.agent-sessions] 已注册".into(),
        ok: reg_ok,
        detail: if reg_ok {
            "注册段存在".into()
        } else {
            "缺失（桌面端重写配置时被冲掉）".into()
        },
    });
    items.push(CheckItem {
        name: "type = \"stdio\" 已声明".into(),
        ok: type_ok,
        detail: if type_ok {
            "stdio 传输".into()
        } else {
            "缺失（Codex ≥0.147 缺此字段静默不加载）".into()
        },
    });
    items.push(CheckItem {
        name: "command 已配置".into(),
        ok: cmd_ok,
        detail: if cmd_ok { "command 存在".into() } else { "缺失".into() },
    });
    items.push(CheckItem {
        name: "args 指向 server.js".into(),
        ok: args_ok,
        detail: if args_ok {
            "server.js 在启动参数中".into()
        } else {
            "缺失（修复将补上指向内置 server.js 的参数）".into()
        },
    });

    // 6. server.js 存在（以 config 实际注册的路径为准，未注册则看内置副本）
    let default_server = server_js_path().to_string_lossy().to_string();
    let effective_server = registered_server.unwrap_or_else(|| default_server.clone());
    let server_js_exists = Path::new(&effective_server).exists();
    items.push(CheckItem {
        name: "server.js 存在".into(),
        ok: server_js_exists,
        detail: if server_js_exists {
            effective_server.clone()
        } else {
            format!("缺失: {effective_server}（修复将改指向内置副本 {default_server}）")
        },
    });

    // 7. 宿主 exe 存在（0.147 需 codex-code-mode-host.exe；从 codex CLI 同目录定位）
    let host_exe = locate_host_exe();
    let host_exe_exists = host_exe.is_some();
    items.push(CheckItem {
        name: "codex-code-mode-host.exe 在位".into(),
        ok: host_exe_exists,
        detail: match &host_exe {
            Some(p) => p.to_string_lossy().to_string(),
            None => "未找到（可用环境变量 HARNESS_CODEX_HOST_EXE 指定路径）".into(),
        },
    });

    // 8. 真实握手（对实际生效的 server.js 发 initialize）
    let handshake_ok = verify_handshake(&effective_server);

    McpStatus {
        config_path: config_path.to_string(),
        items,
        server_js_exists,
        host_exe_exists,
        handshake_ok,
    }
}

/// 真实 MCP 握手：spawn node server.js，发 initialize，期待成功响应
pub fn verify_handshake(server: &str) -> bool {
    let node = std::env::var("HARNESS_NODE_PATH").unwrap_or_else(|_| "node".to_string());
    let server = path_util::strip_verbatim(PathBuf::from(server));
    let mut child = match Command::new(node)
        .arg(&server)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => return false,
    };
    use std::io::Write;
    let init = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{{}}}}}}\n"
    );
    if writeln!(stdin, "{init}").is_err() {
        return false;
    }
    drop(stdin);
    use std::io::BufRead;
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return false,
    };
    let mut ok = false;
    for line in std::io::BufReader::new(stdout).lines() {
        if let Ok(l) = line {
            if l.contains("\"serverInfo\"") {
                ok = true;
                break;
            }
        }
    }
    ok
}

// ---------------- 修复（最小侵入） ----------------

/// 修复缺失项：备份 → 行级插入缺失部分 → 原子写。
/// 保留 config.toml 其他所有内容与注释（不用 toml 序列化往返）。
pub fn fix_mcp(config_path: &str) -> FixResult {
    // 修复时写入的 server.js：三级解析出的内置副本（用户机器上唯一保证存在的）
    let server_js = server_js_path().to_string_lossy().to_string();
    let mut fixed = vec![];
    let raw = match std::fs::read_to_string(config_path) {
        Ok(r) => r,
        Err(e) => {
            return FixResult {
                backup_path: None,
                fixed_items: vec![],
                ok: false,
                message: format!("读取 config 失败: {e}"),
            }
        }
    };
    let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();

    // 备份
    let ts = chrono_like_timestamp();
    let bak = format!("{config_path}.bak-{ts}");
    if std::fs::copy(config_path, &bak).is_err() {
        return FixResult {
            backup_path: None,
            fixed_items: vec![],
            ok: false,
            message: "备份失败，中止修复".into(),
        };
    }

    // 用 toml 解析确认缺什么（结构化判断，避免误插入）
    let parsed = raw.parse::<toml::Value>().ok();
    let section = parsed
        .as_ref()
        .and_then(|v| v.get("mcp_servers"))
        .and_then(|m| m.get("agent-sessions"));
    let missing_section = section.is_none();
    let missing_type = section.and_then(|s| s.get("type")).is_none();
    let missing_cmd = section.and_then(|s| s.get("command")).is_none();
    let missing_args = section
        .and_then(|s| s.get("args"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            !arr.iter().any(|x| {
                x.as_str()
                    .map(|s| s.contains("server.js"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(true);

    if missing_section {
        // 在 [mcp_servers] 段后插入 agent-sessions 子段
        let idx = lines
            .iter()
            .position(|l| l.trim().starts_with("[mcp_servers]"));
        if let Some(i) = idx {
            let block = vec![
                String::new(),
                "[mcp_servers.agent-sessions]".into(),
                "type = \"stdio\"".into(),
                "command = \"node\"".into(),
                format!("args = ['{server_js}']"),
            ];
            // 找段内最后一个非空行（避免插到段中间）
            let mut insert_at = i + 1;
            while insert_at < lines.len()
                && !lines[insert_at].trim().starts_with('[')
                && !lines[insert_at].trim().is_empty()
            {
                insert_at += 1;
            }
            for (k, l) in block.into_iter().enumerate() {
                lines.insert(insert_at + k, l);
            }
            fixed.push("补 [mcp_servers.agent-sessions] 注册段".into());
        } else {
            lines.push(String::new());
            lines.push("[mcp_servers]".into());
            lines.push("[mcp_servers.agent-sessions]".into());
            lines.push("type = \"stdio\"".into());
            lines.push("command = \"node\"".into());
            lines.push(format!("args = ['{server_js}']"));
            fixed.push("追加 [mcp_servers.agent-sessions] 注册段".into());
        }
    } else {
        // 段存在：补缺失字段（在段头行后插入）
        let sec_idx = lines
            .iter()
            .position(|l| l.trim().starts_with("[mcp_servers.agent-sessions]"));
        if let Some(i) = sec_idx {
            let mut insert_at = i + 1;
            if missing_type {
                lines.insert(insert_at, "type = \"stdio\"".into());
                insert_at += 1;
                fixed.push("补 type = \"stdio\"".into());
            }
            if missing_cmd {
                lines.insert(insert_at, "command = \"node\"".into());
                insert_at += 1;
                fixed.push("补 command".into());
            }
            if missing_args {
                lines.insert(insert_at, format!("args = ['{server_js}']"));
                fixed.push("补 args".into());
            }
        }
    }

    // 原子写：先写临时文件再 rename（避免写一半损坏）
    let tmp = format!("{config_path}.tmp");
    let content = lines.join("\n") + "\n";
    match std::fs::write(&tmp, content) {
        Ok(_) => {
            if std::fs::rename(&tmp, config_path).is_err() {
                return FixResult {
                    backup_path: Some(bak),
                    fixed_items: fixed.clone(),
                    ok: false,
                    message: "写入 config 失败".into(),
                };
            }
        }
        Err(e) => {
            return FixResult {
                backup_path: Some(bak),
                fixed_items: fixed,
                ok: false,
                message: format!("写入失败: {e}"),
            }
        }
    }

    let ok = !fixed.is_empty();
    FixResult {
        backup_path: Some(bak),
        fixed_items: fixed.clone(),
        ok,
        message: if ok {
            format!("修复完成：{}（需新开 codex 会话生效）", fixed.join("、"))
        } else {
            "检查通过，无需修复".into()
        },
    }
}

/// 时间戳（不引 chrono，手动拼）
fn chrono_like_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // 转本地时间太麻烦，用自增毫秒后缀保证唯一
    format!("{now}")
}

// ---------------- 测试 ----------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_file(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("mcpcheck-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("config.toml"))
    }

    #[test]
    fn check_healthy_config_passes() {
        let (dir, cfg) = tmp_file("healthy");
        std::fs::write(
            &cfg,
            "model = \"gpt-5.6-luna\"\n\
             [mcp_servers]\n\
             [mcp_servers.agent-sessions]\n\
             type = \"stdio\"\n\
             command = \"node\"\n\
             args = ['C:\\tools\\agent-sessions-mcp\\server.js']\n",
        )
        .unwrap();
        let s = check_mcp(&cfg.to_string_lossy());
        let reg = s.items.iter().find(|i| i.name.contains("已注册")).unwrap();
        let typ = s.items.iter().find(|i| i.name.contains("type")).unwrap();
        assert!(reg.ok);
        assert!(typ.ok);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_missing_section_detected() {
        let (dir, cfg) = tmp_file("missing");
        std::fs::write(&cfg, "model = \"gpt-5.6-luna\"\n[mcp_servers]\n").unwrap();
        let s = check_mcp(&cfg.to_string_lossy());
        let reg = s.items.iter().find(|i| i.name.contains("已注册")).unwrap();
        assert!(!reg.ok);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_missing_type_detected() {
        let (dir, cfg) = tmp_file("notype");
        std::fs::write(
            &cfg,
            "[mcp_servers]\n\
             [mcp_servers.agent-sessions]\n\
             command = \"node\"\n",
        )
        .unwrap();
        let s = check_mcp(&cfg.to_string_lossy());
        let typ = s.items.iter().find(|i| i.name.contains("type")).unwrap();
        assert!(!typ.ok, "缺 type 必须检出: {:?}", typ);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_invalid_toml_reported() {
        let (dir, cfg) = tmp_file("bad");
        std::fs::write(&cfg, "this is not toml [[[").unwrap();
        let s = check_mcp(&cfg.to_string_lossy());
        let parse = s.items.iter().find(|i| i.name.contains("可解析"));
        assert!(parse.is_some() && !parse.unwrap().ok);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_adds_missing_section_preserving_other_content() {
        let (dir, cfg) = tmp_file("fixsec");
        let original = "model = \"gpt-5.6-luna\"\n# 用户注释\n[mcp_servers]\n";
        std::fs::write(&cfg, original).unwrap();
        let r = fix_mcp(&cfg.to_string_lossy());
        assert!(r.ok, "应修复: {:?}", r);
        assert!(r.backup_path.is_some());
        let after = std::fs::read_to_string(&cfg).unwrap();
        assert!(after.contains("[mcp_servers.agent-sessions]"));
        assert!(after.contains("type = \"stdio\""));
        assert!(after.contains("server.js"));
        assert!(after.contains("用户注释"), "其他内容必须保留: {after}");
        // 备份存在
        assert!(Path::new(r.backup_path.as_ref().unwrap()).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_adds_type_when_section_exists() {
        let (dir, cfg) = tmp_file("fixtype");
        std::fs::write(
            &cfg,
            "[mcp_servers]\n\
             [mcp_servers.agent-sessions]\n\
             command = \"node\"\n\
             args = ['server.js']\n",
        )
        .unwrap();
        let r = fix_mcp(&cfg.to_string_lossy());
        assert!(r.ok);
        let after = std::fs::read_to_string(&cfg).unwrap();
        assert!(after.contains("type = \"stdio\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_when_already_healthy_does_nothing() {
        let (dir, cfg) = tmp_file("ok");
        std::fs::write(
            &cfg,
            "[mcp_servers.agent-sessions]\n\
             type = \"stdio\"\n\
             command = \"node\"\n\
             args = ['server.js']\n",
        )
        .unwrap();
        let r = fix_mcp(&cfg.to_string_lossy());
        assert!(!r.ok, "健康时不应改动: {:?}", r);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 默认 config 路径必须动态跟随当前用户 home（而非硬编码某台机器）
    #[test]
    fn default_config_follows_user_home() {
        let fake = std::env::temp_dir().join("fake-home");
        let old_userprofile = std::env::var("USERPROFILE").ok();
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("USERPROFILE", &fake);
        std::env::remove_var("HOME");

        let p = default_config_path();

        match old_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        let expected = fake.join(".codex").join("config.toml");
        assert_eq!(p, expected.to_string_lossy());
    }

    /// fix 写入的必须是三级解析出的内置副本路径（无 mcp-lab 的机器也要可用）
    #[test]
    fn fix_registers_bundled_server_path() {
        let (dir, cfg) = tmp_file("fixpath");
        std::fs::write(&cfg, "[mcp_servers]\n").unwrap();
        let r = fix_mcp(&cfg.to_string_lossy());
        assert!(r.ok, "应修复: {:?}", r);
        let after = std::fs::read_to_string(&cfg).unwrap();
        let bundled = server_js_path().to_string_lossy().to_string();
        assert!(
            after.contains(&bundled),
            "args 应写入内置副本路径 {bundled}: {after}"
        );
        assert!(
            Path::new(&bundled).exists(),
            "内置副本在仓库中必须真实存在: {bundled}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// check 的 server.js 存在性以 config 实际注册路径为准（而非内置常量）
    #[test]
    fn check_uses_registered_server_path() {
        let (dir, cfg) = tmp_file("regpath");
        let missing = r"C:\definitely\not\here\server.js";
        std::fs::write(
            &cfg,
            format!(
                "[mcp_servers.agent-sessions]\n\
                 type = \"stdio\"\n\
                 command = \"node\"\n\
                 args = ['{missing}']\n"
            ),
        )
        .unwrap();
        let s = check_mcp(&cfg.to_string_lossy());
        let item = s.items.iter().find(|i| i.name.contains("server.js 存在")).unwrap();
        assert!(!item.ok, "注册路径不存在必须检出: {:?}", item);
        assert!(
            item.detail.contains("definitely"),
            "detail 应指向实际注册路径: {:?}",
            item.detail
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
