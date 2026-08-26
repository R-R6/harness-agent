//! Stop hook 安装器：把"会话停止 → 把 stdin JSON 追加到 marker 文件"的 hook
//! 幂等写入 ~/.claude/settings.json。引擎靠它拿轮次完成信号（spike B）。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// hook 追加 stdin 到 marker 文件的 PowerShell 命令。两条铁律：
/// 1. 命令里不能出现任何 `$` 变量——Claude Code 执行 hook 时外层 shell 会做
///    变量插值，`$in` 被吞成空导致 ParserError（真实事故一）。
/// 2. stdin/写盘必须显式 UTF-8——默认按控制台代码页(GBK)解码 UTF-8 字节，
///    中文变乱码后整文件对读取端是非法 UTF-8，marker 等于没写（真实事故二：
///    marker 落盘了但引擎读不出，三轮全靠静默兜底）。
///
/// 路径单引号包裹，内部单引号加倍转义。
/// 解释器与 supervise_runner 的 OS 兜底一致：Windows 用自带 powershell 5.1，
/// Unix 用 pwsh（brew install powershell）。不读 HARNESS_PWSH——这条命令会
/// 写进 ~/.claude/settings.json，测试用覆盖不应固化进用户配置。
pub fn hook_command(marker_file: &Path) -> String {
    let path = marker_file.to_string_lossy().replace('\'', "''");
    let shell = if cfg!(windows) { "powershell" } else { "pwsh" };
    format!(
        "{shell} -NoProfile -Command \"[System.IO.File]::AppendAllText('{path}', \
         (New-Object System.IO.StreamReader([Console]::OpenStandardInput(), \
         [System.Text.Encoding]::UTF8)).ReadToEnd(), [System.Text.Encoding]::UTF8)\""
    )
}

/// 确保 settings.json 的 hooks.Stop 含指向 marker 文件的命令。
/// 幂等：已安装且命令与当前 `hook_command` 一致则不动；命令过期则就地改写。
/// 写前备份 settings.json.bak-<epoch 秒>。返回是否发生了写入。
pub fn ensure_stop_hook(settings_path: &Path, marker_file: &Path) -> Result<bool, String> {
    let mut root: Value = match std::fs::read_to_string(settings_path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("settings.json 解析失败（不写入）: {e}"))?,
        Err(_) => json!({}), // 文件不存在：全新安装
    };

    let marker_str = marker_file.to_string_lossy().to_string();
    let hooks = root
        .as_object_mut()
        .ok_or("settings.json 顶层不是对象")?
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or("settings.json 的 hooks 不是对象")?;
    let stop = hooks_obj
        .entry("Stop")
        .or_insert_with(|| json!([]));
    let stop_arr = stop
        .as_array_mut()
        .ok_or("settings.json 的 hooks.Stop 不是数组")?;

    let desired = hook_command(marker_file);
    // 已有指向该 marker 的命令：一致则幂等；不一致则改写。
    // Mac 合并前曾把 `powershell` 写进 settings.json，本机只有 `pwsh`，
    // 不改写的话引擎会一直走静默兜底。
    let mut rewritten = false;
    let mut present = false;
    for entry in stop_arr.iter_mut() {
        let Some(hs) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
            continue;
        };
        for h in hs {
            let Some(c) = h.get("command").and_then(|v| v.as_str()) else {
                continue;
            };
            if !c.contains(&marker_str) {
                continue;
            }
            present = true;
            if c != desired {
                h["command"] = json!(desired.clone());
                rewritten = true;
            }
        }
    }
    if rewritten {
        write_settings(settings_path, &root)?;
        return Ok(true);
    }
    if present {
        return Ok(false);
    }

    stop_arr.push(json!({
        "hooks": [ { "type": "command", "command": desired } ]
    }));

    write_settings(settings_path, &root)?;
    Ok(true)
}

/// 移除指向 marker 文件的 Stop hook 条目（任务结束/应用退出时调用，
/// 防止"装了不卸"：否则用户所有 Claude 会话每次 Stop 都白跑一次这个
/// powershell，多项目监督还会累积多个死条目）。解析失败不动文件。
pub fn remove_stop_hook(settings_path: &Path, marker_file: &Path) -> Result<bool, String> {
    let text = match std::fs::read_to_string(settings_path) {
        Ok(t) => t,
        Err(_) => return Ok(false), // 文件不存在：无需清理
    };
    let mut root: Value = serde_json::from_str(&text)
        .map_err(|e| format!("settings.json 解析失败（不写入）: {e}"))?;

    let marker_str = marker_file.to_string_lossy().to_string();
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok(false);
    };
    let Some(stop) = hooks.get_mut("Stop").and_then(|s| s.as_array_mut()) else {
        return Ok(false);
    };
    let before = stop.len();
    stop.retain(|entry| {
        !entry["hooks"]
            .as_array()
            .map(|hs| {
                hs.iter().any(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|c| c.contains(&marker_str))
                })
            })
            .unwrap_or(false)
    });
    if stop.len() == before {
        return Ok(false);
    }
    if stop.is_empty() {
        hooks.remove("Stop");
    }
    // hooks 变空则一并移除，保持配置干净
    if root.get("hooks").and_then(|h| h.as_object()).is_some_and(|o| o.is_empty()) {
        root.as_object_mut().unwrap().remove("hooks");
    }
    write_settings(settings_path, &root)?;
    Ok(true)
}

/// 备份 + 写回（ensure/remove 共用）
fn write_settings(settings_path: &Path, root: &Value) -> Result<(), String> {
    // 备份（存在才备）
    if settings_path.exists() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let backup: PathBuf = settings_path.with_extension(format!("json.bak-{ts}"));
        std::fs::copy(settings_path, &backup).map_err(|e| format!("备份失败: {e}"))?;
    }
    // 父目录可能不存在（全新机器）
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let text = serde_json::to_string_pretty(root).map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(settings_path, text).map_err(|e| format!("写入失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sv-hook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn installs_into_missing_settings() {
        let dir = tmp("fresh");
        let settings = dir.join("claude").join("settings.json");
        let marker = dir.join("m.jsonl");
        assert!(ensure_stop_hook(&settings, &marker).unwrap(), "应写入");
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        let cmd = v["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(cmd.contains("AppendAllText"), "{cmd}");
        assert!(cmd.contains("UTF8"), "stdin/写盘必须显式 UTF-8（GBK 乱码事故）: {cmd}");
        // 外层 -Command "..." 允许恰好一对引号；内层再出现引号会破坏解析
        assert_eq!(cmd.matches('"').count(), 2, "引号只允许外层一对: {cmd}");
        // 铁律：不能含 $ 变量（外层 shell 插值会吞掉它，真实事故为
        // hook 每次触发每次 ParserError，marker 永远写不出来）
        assert!(!cmd.contains('$'), "命令不得含 $ 变量: {cmd}");
        assert!(cmd.contains(marker.to_string_lossy().as_ref()), "{cmd}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removes_installed_hook_and_keeps_other_settings() {
        let dir = tmp("remove");
        let settings = dir.join("settings.json");
        let marker = dir.join("m.jsonl");
        ensure_stop_hook(&settings, &marker).unwrap();
        // 另一个项目也装了各自的 hook（模拟多项目累积）
        let other_marker = dir.join("other.jsonl");
        ensure_stop_hook(&settings, &other_marker).unwrap();

        assert!(remove_stop_hook(&settings, &marker).unwrap(), "应移除本项目条目");
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1, "另一个项目的 hook 必须保留");
        let cmd = stop[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("other.jsonl"), "{cmd}");

        assert!(remove_stop_hook(&settings, &other_marker).unwrap());
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(v.get("hooks").is_none(), "hooks 清空后应整体移除保持配置干净");
        assert!(!remove_stop_hook(&settings, &other_marker).unwrap(), "重复移除应 no-op");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preserves_existing_settings_and_idempotent() {
        let dir = tmp("existing");
        let settings = dir.join("settings.json");
        let marker = dir.join("m.jsonl");
        std::fs::write(
            &settings,
            r#"{"model": "sonnet", "hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "echo x"}]}]}}"#,
        )
        .unwrap();

        assert!(ensure_stop_hook(&settings, &marker).unwrap(), "首次应写入");
        assert!(!ensure_stop_hook(&settings, &marker).unwrap(), "重复调用应幂等");

        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["model"], "sonnet", "既有配置必须保留");
        assert!(
            v["hooks"]["PreToolUse"].is_array(),
            "既有 hook 必须保留"
        );
        assert!(v["hooks"]["Stop"].is_array());
        // 备份存在
        assert!(dir
            .read_dir()
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().starts_with("settings.json.bak-")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_touch_unparsable_settings() {
        let dir = tmp("bad");
        let settings = dir.join("settings.json");
        std::fs::write(&settings, "not json {").unwrap();
        let err = ensure_stop_hook(&settings, &dir.join("m.jsonl"));
        assert!(err.is_err(), "解析失败必须拒绝写入而不是覆盖用户配置");
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "not json {");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hook_command_uses_platform_shell() {
        let cmd = hook_command(Path::new("/tmp/m.jsonl"));
        #[cfg(windows)]
        assert!(cmd.starts_with("powershell "), "{cmd}");
        #[cfg(not(windows))]
        assert!(cmd.starts_with("pwsh "), "{cmd}");
    }

    #[test]
    fn ensure_stop_hook_rewrites_stale_command() {
        let dir = tmp("migrate");
        let settings = dir.join("settings.json");
        let marker = dir.join("m.jsonl");
        let stale = format!(
            "powershell -NoProfile -Command \"[System.IO.File]::AppendAllText('{}', 'x')\"",
            marker.display()
        );
        std::fs::write(
            &settings,
            json!({
                "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": stale }] }] }
            })
            .to_string(),
        )
        .unwrap();

        assert!(ensure_stop_hook(&settings, &marker).unwrap(), "过期命令应改写");
        assert!(!ensure_stop_hook(&settings, &marker).unwrap(), "改写后应幂等");

        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        let cmd = v["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(cmd, hook_command(&marker), "{cmd}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 生成的命令必须能被本机解释器执行并写出 UTF-8 marker。
    /// 这是 Mac 上 `powershell` 找不到、引擎走静默兜底的回归钉。
    #[cfg(unix)]
    #[test]
    fn hook_command_pwsh_appends_utf8_stdin_to_marker() {
        if std::process::Command::new("pwsh")
            .args(["-NoProfile", "-Command", "exit 0"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("跳过：未找到 pwsh（Unix 需 brew install powershell）");
            return;
        }
        let dir = tmp("pwsh-run");
        let marker = dir.join("m.jsonl");
        let cmd = hook_command(&marker);
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&cmd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn sh");
        {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all("{\"session_id\":\"中文\"}\n".as_bytes())
                .unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "pwsh 失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = std::fs::read_to_string(&marker).expect("marker 应落盘");
        assert!(text.contains("中文"), "UTF-8 内容必须原样写入: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
