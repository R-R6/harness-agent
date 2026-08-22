//! Stop hook 安装器：把"会话停止 → 把 stdin JSON 追加到 marker 文件"的 hook
//! 幂等写入 ~/.claude/settings.json。引擎靠它拿轮次完成信号（spike B）。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// hook 追加 stdin 到 marker 文件的 PowerShell 命令。用 AppendAllText +
/// UTF8Encoding($false)：Add-Content -Encoding UTF8 在 PS5.1 首次建文件时
/// 会写 BOM，导致首个 marker 的 JSON 解析失败、第一轮丢失主信号。
/// 换行用 [char]10 避开内层双引号（外层 -Command 已用双引号包裹）。
/// 路径单引号包裹，内部单引号加倍转义。
pub fn hook_command(marker_file: &Path) -> String {
    let path = marker_file.to_string_lossy().replace('\'', "''");
    format!(
        "powershell -NoProfile -Command \"$in=[Console]::In.ReadToEnd(); \
         [System.IO.File]::AppendAllText('{path}', $in + [char]10, \
         [System.Text.UTF8Encoding]::new($false))\""
    )
}

/// 确保 settings.json 的 hooks.Stop 含指向 marker 文件的命令。
/// 幂等：已安装（任意 hook 命令包含该 marker 路径）则不动。
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

    // 幂等检查：任一现有命令已指向该 marker 文件
    let already = stop_arr.iter().any(|entry| {
        entry["hooks"]
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
    if already {
        return Ok(false);
    }

    stop_arr.push(json!({
        "hooks": [ { "type": "command", "command": hook_command(marker_file) } ]
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
        // 外层 -Command "..." 允许恰好一对引号；内层再出现引号会破坏解析
        assert_eq!(cmd.matches('"').count(), 2, "引号只允许外层一对: {cmd}");
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
}
