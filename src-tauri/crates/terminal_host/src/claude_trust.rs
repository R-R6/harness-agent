//! 在 spawn Claude 之前把工作目录标成已信任，避免 Ink 弹出
//! 「Do you trust this folder?」选择菜单。
//!
//! Harness 添加工作空间时用户已经确认过该目录；Claude Code 另按 cwd 精确路径
//! 记一份 `hasTrustDialogAccepted`，新目录会再弹一次。监督引擎若在菜单还在时
//! 注入任务正文/补回车，Ink 选择器会失去方向键响应（真机：鼠标和方向键都选不了 Yes）。

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

pub fn claude_config_path(home: &Path) -> std::path::PathBuf {
    home.join(".claude.json")
}

/// 把 `work_dir` 及其斜杠变体写入 `projects.<path>.hasTrustDialogAccepted = true`。
/// 已是 true 则原样返回。配置损坏时返回 Err，调用方不应覆盖原文件。
pub fn ensure_folder_trusted(config_path: &Path, work_dir: &str) -> Result<(), String> {
    let keys = project_keys(work_dir);
    if keys.is_empty() {
        return Ok(());
    }

    let mut root = if config_path.is_file() {
        let bytes = fs::read(config_path).map_err(|e| format!("读取 Claude 配置失败: {e}"))?;
        if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|e| format!("解析 Claude 配置失败（未改写）: {e}"))?
        }
    } else {
        json!({})
    };
    if !root.is_object() {
        return Err("Claude 配置根节点不是对象，未改写".into());
    }

    if root.get("projects").is_none() {
        root["projects"] = json!({});
    }
    let projects = root
        .get_mut("projects")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| "Claude 配置 projects 不是对象，未改写".to_string())?;

    let mut changed = false;
    // Claude 按 cwd 字符串精确匹配；Windows 大小写不同即视为另一条。
    // 已有条目（例如 F:\…\Amazing\test1）也要标上，避免小写 work_dir 漏标后再次弹框。
    let existing: Vec<String> = projects.keys().cloned().collect();
    for key in existing {
        if !(path_key_eq(&key, work_dir) || keys.iter().any(|k| path_key_eq(k, &key))) {
            continue;
        }
        match projects.get_mut(&key) {
            Some(entry) if entry.is_object() => {
                if entry.get("hasTrustDialogAccepted") != Some(&Value::Bool(true)) {
                    entry["hasTrustDialogAccepted"] = Value::Bool(true);
                    changed = true;
                }
            }
            Some(_) => {
                return Err(format!("Claude 配置 projects.{key} 不是对象，未改写"));
            }
            None => {}
        }
    }
    for key in &keys {
        if projects.contains_key(key) {
            continue;
        }
        projects.insert(key.clone(), default_trusted_project());
        changed = true;
    }

    if !changed {
        return Ok(());
    }

    let data = serde_json::to_vec_pretty(&root).map_err(|e| format!("序列化 Claude 配置失败: {e}"))?;
    fs::write(config_path, data).map_err(|e| format!("写入 Claude 配置失败: {e}"))?;
    Ok(())
}

fn default_trusted_project() -> Value {
    json!({
        "allowedTools": [],
        "mcpContextUris": [],
        "enabledMcpjsonServers": [],
        "disabledMcpjsonServers": [],
        "hasTrustDialogAccepted": true,
        "hasClaudeMdExternalIncludesApproved": false,
        "hasClaudeMdExternalIncludesWarningShown": false
    })
}

fn path_key_eq(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        s.trim()
            .trim_end_matches(['/', '\\'])
            .replace('/', "\\")
            .to_ascii_lowercase()
    }
    let a = norm(a);
    let b = norm(b);
    !a.is_empty() && a == b
}

/// Claude 配置键：正/反斜杠 + 大小写 + 盘符大小写都写上（Windows cwd 字符串精确匹配）。
fn project_keys(work_dir: &str) -> Vec<String> {
    let trimmed = work_dir.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut keys = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && !keys.iter().any(|k| k == &s) {
            keys.push(s);
        }
    };
    let slash = trimmed.replace('\\', "/");
    let back = trimmed.replace('/', "\\");
    for base in [trimmed, slash.as_str(), back.as_str()] {
        push(base.to_string());
        push(base.to_ascii_lowercase());
        if let Some(drive) = with_upper_drive(base) {
            push(drive);
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

fn with_upper_drive(s: &str) -> Option<String> {
    let mut chars = s.chars();
    let drive = chars.next()?;
    let colon = chars.next()?;
    if colon == ':' && drive.is_ascii_alphabetic() {
        Some(format!("{}:{}", drive.to_ascii_uppercase(), chars.as_str()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_cfg() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("ha-claude-trust-{nanos}.json"))
    }

    #[test]
    fn creates_project_entry_and_marks_trusted() {
        let path = temp_cfg();
        ensure_folder_trusted(&path, r"F:\project\workspace-side\Amazing\test2").unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let slash = v["projects"]["F:/project/workspace-side/Amazing/test2"]["hasTrustDialogAccepted"]
            .as_bool();
        let back = v["projects"][r"F:\project\workspace-side\Amazing\test2"]["hasTrustDialogAccepted"]
            .as_bool();
        assert_eq!(slash, Some(true));
        assert_eq!(back, Some(true));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn does_not_wipe_existing_project_fields() {
        let path = temp_cfg();
        fs::write(
            &path,
            r#"{
  "projects": {
    "F:\\work\\app": {
      "allowedTools": ["Bash"],
      "hasTrustDialogAccepted": false,
      "lastCost": 1.5
    }
  }
}"#,
        )
        .unwrap();
        ensure_folder_trusted(&path, r"F:\work\app").unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let entry = &v["projects"][r"F:\work\app"];
        assert_eq!(entry["hasTrustDialogAccepted"], true);
        assert_eq!(entry["lastCost"], 1.5);
        assert_eq!(entry["allowedTools"][0], "Bash");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn refuses_to_overwrite_corrupt_config() {
        let path = temp_cfg();
        fs::write(&path, "not-json").unwrap();
        let err = ensure_folder_trusted(&path, r"F:\x").unwrap_err();
        assert!(err.contains("未改写"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "not-json");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn lowercase_work_dir_marks_existing_mixed_case_entry() {
        let path = temp_cfg();
        fs::write(
            &path,
            r#"{
  "projects": {
    "F:\\project\\workspace-side\\Amazing\\test1": {
      "hasTrustDialogAccepted": false,
      "lastCost": 2
    }
  }
}"#,
        )
        .unwrap();
        ensure_folder_trusted(&path, r"f:\project\workspace-side\amazing\test1").unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let existing = &v["projects"][r"F:\project\workspace-side\Amazing\test1"];
        assert_eq!(existing["hasTrustDialogAccepted"], true);
        assert_eq!(existing["lastCost"], 2);
        assert_eq!(
            v["projects"][r"f:\project\workspace-side\amazing\test1"]["hasTrustDialogAccepted"],
            true
        );
        let _ = fs::remove_file(path);
    }
}
