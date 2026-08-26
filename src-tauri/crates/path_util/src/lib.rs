//! path_util —— session_proxy / supervise_runner / mcp_checker 共用的脚本路径定位工具
//!
//! 脚本（server.js / supervise.ps1）随应用打包在 resources/ 下，定位规则三级：
//! 1. 环境变量覆盖（测试/调试）
//! 2. tauri setup 注入的打包资源路径（发布/开发，来自 BaseDirectory::Resource）
//! 3. 兜底：仓库内副本（cargo test 无注入时的默认值）

use std::path::PathBuf;

/// 三级解析脚本路径：环境变量 → 注入值 → 仓库内兜底，末尾统一剥 verbatim 前缀
pub fn resolve_script(env_key: &str, injected: Option<&PathBuf>, dev_fallback: &str) -> PathBuf {
    let p = if let Ok(v) = std::env::var(env_key) {
        PathBuf::from(v)
    } else if let Some(p) = injected {
        p.to_path_buf()
    } else {
        normalize_dev_path(dev_fallback)
    };
    strip_verbatim(p)
}

/// 消除路径中的 . / .. 冗余组件（保留盘符大小写）。
/// 不用 canonicalize：Windows 下它返回 \\?\ verbatim 路径，下游 node 会解析失败
pub fn normalize_dev_path(raw: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for c in PathBuf::from(raw).components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 去掉 Windows verbatim 前缀（\\?\）：node 的 realpathSync 会把 \\?\F:\... 拆碎成
/// 盘符段 F:（报 EISDIR）。GNU 工具链下 current_exe / canonicalize 可能产生该前缀。
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    if cfg!(windows) {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix("\\\\?\\") {
            return PathBuf::from(rest);
        }
    }
    p
}

/// GUI 进程（发布版 windows_subsystem="windows"）spawn 控制台程序时必须加
/// CREATE_NO_WINDOW，否则每次 spawn node/pwsh/taskkill/codex 都会弹出一个
/// 可见控制台窗口（开发版是 console 子系统所以看不出来）。其他平台 no-op。
pub fn no_console_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows 语义：\\?\ 前缀剥离。Unix 上 cfg!(windows) 为假，
    /// strip_verbatim 原样返回，故只在 Windows 跑这对断言
    #[cfg(windows)]
    #[test]
    fn strip_verbatim_removes_windows_prefix() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\F:\proj\server.js")),
            PathBuf::from(r"F:\proj\server.js")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"F:\proj\server.js")),
            PathBuf::from(r"F:\proj\server.js")
        );
    }

    /// Unix 语义：无 verbatim 概念，原样返回
    #[cfg(not(windows))]
    #[test]
    fn strip_verbatim_returns_unchanged_on_unix() {
        assert_eq!(
            strip_verbatim(PathBuf::from("/tmp/x/server.js")),
            PathBuf::from("/tmp/x/server.js")
        );
    }

    /// Windows 反斜杠路径的 .. 折叠（'\' 在 Unix 不是分隔符，整串成一个组件，
    /// 无法折叠——只在 Windows 断言）
    #[cfg(windows)]
    #[test]
    fn normalize_dev_path_collapses_parent_dirs() {
        let p = normalize_dev_path(r"F:\repo\src-tauri\crates\x\..\..\resources\s.js");
        assert_eq!(p, PathBuf::from(r"F:\repo\src-tauri\resources\s.js"));
    }

    /// 正斜杠路径的 .. 折叠（Windows 的 Path 解析同样接受 '/'，双平台通用）
    #[test]
    fn normalize_dev_path_collapses_parent_dirs_posix() {
        let p = normalize_dev_path("/repo/src-tauri/crates/x/../../resources/s.js");
        assert_eq!(p, PathBuf::from("/repo/src-tauri/resources/s.js"));
    }

    /// 环境变量优先级最高，其次注入值，最后兜底
    #[test]
    fn resolve_script_tier_precedence() {
        let key = "HARNESS_TEST_PATH_UTIL_KEY";
        let injected = PathBuf::from(r"C:\injected\s.js");
        let fallback = "C:\\fallback\\s.js";

        std::env::set_var(key, r"C:\env-override\s.js");
        assert_eq!(
            resolve_script(key, Some(&injected), fallback),
            PathBuf::from(r"C:\env-override\s.js")
        );
        std::env::remove_var(key);

        assert_eq!(
            resolve_script(key, Some(&injected), fallback),
            PathBuf::from(r"C:\injected\s.js")
        );
        assert_eq!(
            resolve_script(key, None, fallback),
            PathBuf::from(r"C:\fallback\s.js")
        );
    }
}
