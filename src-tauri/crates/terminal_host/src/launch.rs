use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

// Windows npm 目录勘察工具：调用方全在 cfg(windows) 分支，Unix 构建下为死代码
#[cfg_attr(not(windows), allow(dead_code))]
const NATIVE_BINARY_MIN_BYTES: u64 = 4096;

pub fn terminal_command(agent: &str, extra_args: &[String]) -> Result<(String, Vec<String>), String> {
    #[cfg(windows)]
    {
        match agent {
            "claude" => windows_claude_command(extra_args),
            "codex" => windows_cmd_agent("codex", extra_args),
            other => Err(format!("不支持的终端 Agent: {other}")),
        }
    }
    #[cfg(not(windows))]
    {
        match agent {
            "claude" | "codex" => Ok((agent.to_string(), extra_args.to_vec())),
            other => Err(format!("不支持的终端 Agent: {other}")),
        }
    }
}

#[cfg(windows)]
fn comspec() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
}

#[cfg(windows)]
fn windows_cmd_agent(name: &str, extra_args: &[String]) -> Result<(String, Vec<String>), String> {
    // Claude/Codex 在 Windows 上都是 npm 装的 .cmd shim。走 cmd.exe 让 PATHEXT
    // 解析出 .cmd，而不是直接 CreateProcessW 裸名——裸名会先命中 npm 全局目录里
    // 那个无扩展名的 sh 脚本（#!/bin/sh，不是 Win32 程序），报 ERROR_BAD_EXE_FORMAT
    // (os error 193)。cmd.exe 同时转发 stdin/stdout/ANSI/Ctrl+C。续聊等附加参数
    // 追加在 CLI 名之后，由 cmd /c 原样转发。
    let mut args = vec!["/d".into(), "/s".into(), "/c".into(), name.to_string()];
    args.extend(extra_args.iter().cloned());
    Ok((comspec(), args))
}

#[cfg(windows)]
fn windows_claude_command(extra_args: &[String]) -> Result<(String, Vec<String>), String> {
    if let Some(shim) = find_on_path("claude.cmd") {
        if let Some(prefix) = shim.parent() {
            return windows_claude_from_prefix(prefix, extra_args);
        }
    }
    windows_cmd_agent("claude", extra_args)
}

#[cfg(windows)]
fn windows_claude_from_prefix(prefix: &Path, extra_args: &[String]) -> Result<(String, Vec<String>), String> {
    if let Some(exe) = find_claude_native_exe(prefix) {
        return Ok((exe.to_string_lossy().into_owned(), extra_args.to_vec()));
    }

    let stub = prefix.join(r"node_modules\@anthropic-ai\claude-code\bin\claude.exe");
    if stub.is_file() && !looks_like_windows_pe(&stub) {
        return Err(
            "Claude Code 原生程序未安装：当前 claude.exe 只是 npm 占位脚本，Windows 无法运行。请在本机执行 npm install -g @anthropic-ai/claude-code".into(),
        );
    }

    windows_cmd_agent("claude", extra_args)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn looks_like_windows_pe(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if metadata.len() < NATIVE_BINARY_MIN_BYTES {
        return false;
    }
    let mut magic = [0u8; 2];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .map(|_| magic == *b"MZ")
        .unwrap_or(false)
}

#[cfg(windows)]
fn find_claude_native_exe(prefix: &Path) -> Option<PathBuf> {
    let candidates = [
        prefix.join(r"node_modules\@anthropic-ai\claude-code\bin\claude.exe"),
        prefix.join(r"node_modules\@anthropic-ai\claude-code-win32-x64\claude.exe"),
    ];
    for candidate in candidates {
        if looks_like_windows_pe(&candidate) {
            return Some(candidate);
        }
    }

    let vendor = prefix.join(r"node_modules\@anthropic-ai");
    let entries = std::fs::read_dir(vendor).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(".claude-code-") {
            continue;
        }
        let nested = entry
            .path()
            .join(r"node_modules\@anthropic-ai\claude-code-win32-x64\claude.exe");
        if looks_like_windows_pe(&nested) {
            return Some(nested);
        }
    }
    None
}

#[cfg_attr(not(windows), allow(dead_code))]
fn find_on_path(file_name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(file_name);
        candidate.is_file().then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_prefix() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ha-claude-launch-{nanos}"));
        fs::create_dir_all(&dir).expect("temp prefix");
        dir
    }

    fn write_bytes(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, bytes).expect("write");
    }

    fn fake_pe() -> Vec<u8> {
        let mut bytes = b"MZ".to_vec();
        bytes.resize(NATIVE_BINARY_MIN_BYTES as usize + 8, 0);
        bytes
    }

    #[test]
    fn rejects_install_stub_as_pe() {
        let prefix = temp_prefix();
        let stub = prefix.join(r"node_modules\@anthropic-ai\claude-code\bin\claude.exe");
        write_bytes(&stub, b"echo \"Error: claude native binary not installed.\"\nexit 1\n");
        assert!(!looks_like_windows_pe(&stub));
        let _ = fs::remove_dir_all(prefix);
    }

    #[test]
    fn accepts_mz_payload_as_pe() {
        let prefix = temp_prefix();
        let exe = prefix.join("claude.exe");
        write_bytes(&exe, &fake_pe());
        assert!(looks_like_windows_pe(&exe));
        let _ = fs::remove_dir_all(prefix);
    }

    // fixture 用反斜杠拼 Windows npm 目录布局：'\' 在 Unix 不是路径分隔符，
    // 整串塌缩成单文件名，vendor 目录扫描必失败——只在 Windows 跑
    #[cfg(windows)]
    #[test]
    fn finds_native_binary_in_interrupted_npm_extract() {
        let prefix = temp_prefix();
        let stub = prefix.join(r"node_modules\@anthropic-ai\claude-code\bin\claude.exe");
        write_bytes(&stub, b"echo stub\nexit 1\n");
        let leftover = prefix.join(
            r"node_modules\@anthropic-ai\.claude-code-SFgM2ooi\node_modules\@anthropic-ai\claude-code-win32-x64\claude.exe",
        );
        write_bytes(&leftover, &fake_pe());

        let found = find_claude_native_exe(&prefix).expect("leftover pe");
        assert_eq!(found, leftover);
        let _ = fs::remove_dir_all(prefix);
    }

    #[cfg(windows)]
    #[test]
    fn launches_leftover_native_binary_instead_of_stub() {
        let prefix = temp_prefix();
        let stub = prefix.join(r"node_modules\@anthropic-ai\claude-code\bin\claude.exe");
        write_bytes(&stub, b"echo stub\nexit 1\n");
        let leftover = prefix.join(
            r"node_modules\@anthropic-ai\.claude-code-tmp\node_modules\@anthropic-ai\claude-code-win32-x64\claude.exe",
        );
        write_bytes(&leftover, &fake_pe());

        let extra = vec!["--resume".to_string(), "session-42".to_string()];
        let (command, args) = windows_claude_from_prefix(&prefix, &extra).expect("launch leftover");
        assert_eq!(Path::new(&command), leftover.as_path());
        assert_eq!(args, extra, "原生 exe 的附加参数应原样透传");
        let _ = fs::remove_dir_all(prefix);
    }

    /// cmd.exe 路径的附加参数追加在 CLI 名之后（续聊：codex resume <id>）
    #[cfg(windows)]
    #[test]
    fn cmd_agent_appends_extra_args_after_cli_name() {
        let (command, args) =
            windows_cmd_agent("codex", &["resume".into(), "abc-123".into()]).expect("cmd args");
        assert!(command.to_lowercase().ends_with("cmd.exe"), "command: {command}");
        assert_eq!(
            args,
            vec![
                "/d".to_string(),
                "/s".to_string(),
                "/c".to_string(),
                "codex".to_string(),
                "resume".to_string(),
                "abc-123".to_string(),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn errors_clearly_when_only_the_install_stub_exists() {
        let prefix = temp_prefix();
        let stub = prefix.join(r"node_modules\@anthropic-ai\claude-code\bin\claude.exe");
        write_bytes(&stub, b"echo stub\nexit 1\n");
        let error = windows_claude_from_prefix(&prefix, &[]).expect_err("stub must fail");
        assert!(error.contains("原生程序未安装"), "{error}");
        let _ = fs::remove_dir_all(prefix);
    }
}
