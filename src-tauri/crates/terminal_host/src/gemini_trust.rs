//! Gemini CLI 的目录信任：写 `~/.gemini/trustedFolders.json`。
//!
//! Gemini CLI 首次进入未信任目录会弹信任对话框（headless 直接报
//! FatalUntrustedWorkspaceError）。Harness 添加工作空间时用户已确认过目录，
//! 预写信任可避免交互 pane 冷启动被菜单卡住（与 claude_trust 同一思路）。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

pub fn gemini_trusted_folders_path(home: &Path) -> std::path::PathBuf {
    home.join(".gemini").join("trustedFolders.json")
}

/// 把 `work_dir`（含反斜杠→斜杠变体）标记为信任：`{"<path>": true, ...}`。
/// 已有条目保留其他字段；损坏文件返回 Err 不覆写。
///
/// 信任值为 Gemini 的枚举字符串（TRUST_FOLDER/TRUST_PARENT/DO_NOT_TRUST），
/// 不是布尔——真机教训：写 `true` 会让 gemini 启动即报
/// `Invalid trust level "true"`（老版本文档的布尔写法已废弃）。
/// 兼容迁移：读到布尔 true/false 时升级为 TRUST_FOLDER/DO_NOT_TRUST。
pub fn ensure_gemini_folder_trusted(config_path: &Path, work_dir: &str) -> Result<(), String> {
    const TRUSTED: &str = "TRUST_FOLDER";
    const NOT_TRUSTED: &str = "DO_NOT_TRUST";
    let trimmed = work_dir.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Ok(());
    }

    let mut root: BTreeMap<String, Value> = if config_path.is_file() {
        let bytes = fs::read(config_path).map_err(|e| format!("读取 Gemini 信任配置失败: {e}"))?;
        if bytes.is_empty() {
            BTreeMap::new()
        } else {
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|e| format!("解析 Gemini 信任配置失败（未改写）: {e}"))?
                .as_object()
                .ok_or_else(|| "Gemini 信任配置根节点不是对象，未改写".to_string())?
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }
    } else {
        BTreeMap::new()
    };

    // Gemini 按 cwd 字符串精确匹配；正反斜杠都写上（Windows 大小写按原样保留）。
    // 顺带迁移旧布尔值（历史版本 bug 写入的 true/false）
    let slash = trimmed.replace('\\', "/");
    let mut changed = false;
    for key in [trimmed.to_string(), slash] {
        match root.get(&key) {
            Some(Value::String(s)) if s == TRUSTED => {}
            Some(Value::Bool(true)) => {
                // 旧版 bug：布尔 true 被新 gemini 拒绝，升级为枚举
                root.insert(key, Value::String(TRUSTED.into()));
                changed = true;
            }
            Some(Value::Bool(false)) => {
                root.insert(key, Value::String(NOT_TRUSTED.into()));
                changed = true;
            }
            Some(_) => {
                return Err(format!(
                    "Gemini 信任配置 {key} 不是合法信任值（TRUST_FOLDER/TRUST_PARENT/DO_NOT_TRUST），未改写"
                ));
            }
            None => {
                root.insert(key, Value::String(TRUSTED.into()));
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(());
    }

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 .gemini 目录失败: {e}"))?;
    }
    let data =
        serde_json::to_vec_pretty(&root).map_err(|e| format!("序列化 Gemini 信任配置失败: {e}"))?;
    fs::write(config_path, data).map_err(|e| format!("写入 Gemini 信任配置失败: {e}"))?;
    Ok(())
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
        std::env::temp_dir().join(format!("ha-gemini-trust-{nanos}.json"))
    }

    #[test]
    fn marks_both_slash_variants_with_enum_string() {
        let path = temp_cfg();
        ensure_gemini_folder_trusted(&path, r"F:\work\proj").unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        // gemini 信任值是枚举字符串（TRUST_FOLDER）——布尔 true 会被 CLI 拒绝
        assert_eq!(v[r"F:\work\proj"], Value::String("TRUST_FOLDER".into()));
        assert_eq!(v["F:/work/proj"], Value::String("TRUST_FOLDER".into()));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn migrates_legacy_boolean_true() {
        // 真机事故：旧版写 true 被 gemini 拒绝（Invalid trust level "true"）
        let path = temp_cfg();
        fs::write(&path, r#"{"D:\\other": true, "keep": "value"}"#).unwrap();
        ensure_gemini_folder_trusted(&path, r"D:\other").unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(v[r"D:\other"], Value::String("TRUST_FOLDER".into()));
        assert_eq!(v["keep"], Value::from("value"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn idempotent_when_already_enum_trusted() {
        let path = temp_cfg();
        fs::write(&path, r#"{"D:\\other": "TRUST_FOLDER", "keep": "value"}"#).unwrap();
        ensure_gemini_folder_trusted(&path, r"D:\other").unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(v[r"D:\other"], Value::String("TRUST_FOLDER".into()));
        assert_eq!(v["keep"], Value::from("value"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn refuses_invalid_trust_value() {
        let path = temp_cfg();
        fs::write(&path, r#"{"D:\\other": "WEIRD"}"#).unwrap();
        let err = ensure_gemini_folder_trusted(&path, r"D:\other").unwrap_err();
        assert!(err.contains("未改写"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"D:\\other": "WEIRD"}"#);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn refuses_corrupt_config() {
        let path = temp_cfg();
        fs::write(&path, "not-json").unwrap();
        let err = ensure_gemini_folder_trusted(&path, r"F:\x").unwrap_err();
        assert!(err.contains("未改写"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "not-json");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn creates_parent_dir_and_trims_trailing_slash() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let nested = std::env::temp_dir().join(format!("ha-gemini-{nanos}"));
        let path = nested.join(".gemini").join("trustedFolders.json");
        ensure_gemini_folder_trusted(&path, r"F:\work\proj\").unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(v[r"F:\work\proj"], Value::String("TRUST_FOLDER".into()));
        let _ = fs::remove_dir_all(nested);
    }
}
