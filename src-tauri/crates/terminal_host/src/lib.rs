//! Native PTY primitives for interactive local CLI sessions.
//!
//! This crate deliberately knows nothing about Tauri, React or model APIs. It
//! only creates a ConPTY-backed child, exposes byte streams, resizes the PTY,
//! and provides a kill/wait boundary for the application process manager.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

mod launch;
pub use launch::terminal_command;

pub struct SpawnedTerminal {
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub child: Arc<Mutex<Box<dyn Child + Send>>>,
    pub reader: Box<dyn Read + Send>,
    pub pid: Option<u32>,
}

pub fn spawn(
    command: &str,
    args: &[String],
    work_dir: &str,
    cols: u16,
    rows: u16,
) -> Result<SpawnedTerminal, String> {
    if command.trim().is_empty() {
        return Err("CLI 可执行文件不能为空".into());
    }
    if work_dir.trim().is_empty() {
        return Err("工作目录不能为空".into());
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(2),
            cols: cols.max(20),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("创建 PTY 失败: {error}"))?;

    let mut cmd = CommandBuilder::new(command);
    cmd.args(args);
    cmd.cwd(work_dir);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|error| format!("启动 CLI 失败: {error}"))?;
    drop(pair.slave);

    let pid = child.process_id();
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| format!("读取 PTY 输出失败: {error}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| format!("打开 PTY 输入失败: {error}"))?;

    Ok(SpawnedTerminal {
        master: Arc::new(Mutex::new(pair.master)),
        writer: Arc::new(Mutex::new(writer)),
        child: Arc::new(Mutex::new(child)),
        reader,
        pid,
    })
}

pub fn write_input(writer: &Arc<Mutex<Box<dyn Write + Send>>>, data: &[u8]) -> Result<(), String> {
    let mut stream = writer.lock().map_err(|_| "PTY 输入锁已损坏".to_string())?;
    stream
        .write_all(data)
        .and_then(|_| stream.flush())
        .map_err(|error| format!("写入 PTY 失败: {error}"))
}

pub fn resize(
    master: &Arc<Mutex<Box<dyn MasterPty + Send>>>,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let pty = master.lock().map_err(|_| "PTY 尺寸锁已损坏".to_string())?;
    pty.resize(PtySize {
        rows: rows.max(2),
        cols: cols.max(20),
        pixel_width: 0,
        pixel_height: 0,
    })
    .map_err(|error| format!("调整 PTY 尺寸失败: {error}"))
}

pub fn kill(child: &Arc<Mutex<Box<dyn Child + Send>>>) -> Result<(), String> {
    let mut process = child.lock().map_err(|_| "PTY 进程锁已损坏".to_string())?;
    process.kill().map_err(|error| format!("停止 CLI 失败: {error}"))
}

pub fn wait(child: &Arc<Mutex<Box<dyn Child + Send>>>) -> Option<i32> {
    let mut process = child.lock().ok()?;
    process.wait().ok().map(|status| status.exit_code() as i32)
}
