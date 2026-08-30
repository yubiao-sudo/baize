//! 集成终端（PTY）：一个独立的「白泽终端」窗口，支持用户与白泽直接在终端里输入命令执行。
//!
//! 采用 `portable-pty` 提供真实伪终端：
//! - Windows 走 ConPTY（PowerShell 交互式、支持 UTF-8 / ANSI 颜色 / 提示符）
//! - 读取端在后台线程持续读，通过 `term-data` 事件推给前端 xterm.js 渲染
//! - 写入端由前端按键（term_write）或白泽工具（terminal_send）调用
//! - terminal_send 执行命令后会捕获终端输出，返回给白泽判断命令是否成功

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::tools::{PermissionClass, Tool};

/// 终端共享状态：最多一个会话（单终端窗口）
pub struct TerminalState {
    session: Mutex<Option<TermSession>>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct TermSession {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    stop: Arc<AtomicBool>,
    /// 共享输出缓冲区：读取线程持续写入，terminal_send 从中捕获命令输出
    output_buf: Arc<Mutex<String>>,
}

impl TerminalState {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
            reader: Mutex::new(None),
        }
    }

    pub fn is_running(&self) -> bool {
        self.session.lock().unwrap().is_some()
    }

    /// 启动交互式 shell（幂等：已存在直接返回）
    pub fn spawn(&self, app: AppHandle) -> Result<(), String> {
        if self.is_running() {
            return Ok(());
        }

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: 24,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("打开 PTY 失败: {e}"))?;

        let mut cmd = CommandBuilder::new(shell());
        #[cfg(windows)]
        cmd.arg("-NoLogo");
        #[cfg(not(windows))]
        cmd.arg("-i");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("启动终端 shell 失败: {e}"))?;
        // 释放 slave 端，master 端负责后续读写
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("克隆读取端失败: {e}"))?;
        let writer = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|e| format!("获取写入端失败: {e}"))?,
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let output_buf = Arc::new(Mutex::new(String::new()));

        let stop_reader = stop.clone();
        let app_reader = app.clone();
        let output_buf_reader = output_buf.clone();
        let reader = std::thread::spawn(move || {
            let mut r = reader;
            let mut buf = [0u8; 8192];
            while !stop_reader.load(Ordering::Relaxed) {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).to_string();
                        // 推送到前端渲染
                        let _ = app_reader.emit("term-data", text.clone());
                        // 同时写入共享缓冲区（供 terminal_send 捕获输出）
                        if let Ok(mut out) = output_buf_reader.lock() {
                            out.push_str(&text);
                            // 限制最大 200KB，避免无限增长
                            if out.len() > 200_000 {
                                let keep = out.len().saturating_sub(100_000);
                                *out = out[keep..].to_string();
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        *self.reader.lock().unwrap() = Some(reader);
        *self.session.lock().unwrap() = Some(TermSession {
            master: pair.master,
            writer,
            child,
            stop,
            output_buf,
        });
        Ok(())
    }

    /// 向终端写入原始字符（用于前端按键回送，或白泽注入命令）
    pub fn write(&self, data: &str) -> Result<(), String> {
        let writer = {
            let guard = self.session.lock().unwrap();
            let s = guard.as_ref().ok_or("终端未启动")?;
            s.writer.clone()
        };
        let mut w = writer.lock().unwrap();
        w.write_all(data.as_bytes())
            .map_err(|e| format!("终端写入失败: {e}"))
    }

    /// 清空共享输出缓冲区（发送命令前调用，以便捕获该命令的专属输出）
    pub fn clear_output_buf(&self) {
        if let Some(s) = self.session.lock().unwrap().as_ref() {
            if let Ok(mut buf) = s.output_buf.lock() {
                buf.clear();
            }
        }
    }

    /// 读取共享输出缓冲区快照
    pub fn read_output_buf(&self) -> String {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.output_buf.lock().ok())
            .map(|b| b.clone())
            .unwrap_or_default()
    }

    /// 发送命令并等待捕获输出：写入命令 → 轮询输出缓冲区直到稳定或超时 → 返回输出
    pub fn send_and_capture(&self, cmd: &str, timeout_ms: u64) -> Result<String, String> {
        self.clear_output_buf();
        self.write(&format!("{}\r", cmd))?;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut last_len = 0usize;
        let mut stable_ms = 0u64;

        loop {
            std::thread::sleep(Duration::from_millis(200));
            let current = self.read_output_buf();
            let now = Instant::now();

            if current.len() == last_len {
                stable_ms += 200;
                // 输出连续 800ms 不变，认为命令已执行完毕
                if stable_ms >= 800 {
                    return Ok(current);
                }
            } else {
                stable_ms = 0;
                last_len = current.len();
            }

            if now >= deadline {
                return Ok(current);
            }
        }
    }

    /// 调整终端行列尺寸
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        let guard = self.session.lock().unwrap();
        let s = guard.as_ref().ok_or("终端未启动")?;
        s.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("调整终端尺寸失败: {e}"))
    }

    /// 结束会话：停止读取线程、杀掉子进程、释放会话。
    /// 全部清理都放到后台线程执行：Windows ConPTY 的 `kill`/`drop` 可能阻塞
    /// （等待子进程退出 / 关闭管道），若在调用方（常为窗口关闭事件主线程）同步执行，
    /// 会冻结整个界面（表现为设置打不开、对话无响应）。
    pub fn close(&self) {
        let session = self.session.lock().unwrap().take();
        let reader = self.reader.lock().unwrap().take();
        std::thread::spawn(move || {
            if let Some(mut s) = session {
                s.stop.store(true, Ordering::Relaxed);
                let _ = s.child.kill();
                // 显式丢弃 master/writer，促使读取线程尽快拿到 EOF
                drop(s);
            }
            if let Some(h) = reader {
                let _ = h.join();
            }
        });
    }
}

fn shell() -> &'static str {
    #[cfg(windows)]
    {
        "powershell.exe"
    }
    #[cfg(not(windows))]
    {
        "bash"
    }
}

// ---------------- 工具：打开终端窗口 ----------------

pub struct OpenTerminalTool {
    app: AppHandle,
    terminal: Arc<TerminalState>,
}

impl OpenTerminalTool {
    pub fn new(app: AppHandle, terminal: Arc<TerminalState>) -> Self {
        Self { app, terminal }
    }
}

impl Tool for OpenTerminalTool {
    fn name(&self) -> &str {
        "open_terminal"
    }
    fn description(&self) -> &str {
        "打开内置「白泽终端」窗口并启动交互式 shell。之后可用 terminal_send 向其中输入命令执行。"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        crate::windows::ensure_terminal_window(&self.app, self.terminal.clone());
        self.terminal.spawn(self.app.clone())?;
        Ok(json!({ "ok": true }))
    }
}

// ---------------- 工具：向终端发送命令执行 ----------------

pub struct TerminalSendTool {
    app: AppHandle,
    terminal: Arc<TerminalState>,
}

impl TerminalSendTool {
    pub fn new(app: AppHandle, terminal: Arc<TerminalState>) -> Self {
        Self { app, terminal }
    }
}

impl Tool for TerminalSendTool {
    fn name(&self) -> &str {
        "terminal_send"
    }
    fn description(&self) -> &str {
        "向内置「白泽终端」发送一条命令并回车执行，等待命令完成后返回终端输出。\
         白泽可据此判断命令是否成功，若失败则分析错误原因并尝试纠正后重新执行。\
         缺省先自动打开终端窗口。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要在终端里执行的命令" },
                "timeout_ms": {
                    "type": "integer",
                    "description": "等待命令输出的超时毫秒数（默认 10000，即 10 秒；长时间命令可调大）",
                    "default": 10000
                }
            },
            "required": ["command"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let cmd = args["command"].as_str().ok_or("缺少参数 command")?;
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(10000).clamp(1000, 120_000);
        crate::windows::ensure_terminal_window(&self.app, self.terminal.clone());
        if !self.terminal.is_running() {
            self.terminal.spawn(self.app.clone())?;
        }
        let output = self.terminal.send_and_capture(cmd, timeout_ms)?;
        Ok(json!({ "ok": true, "command": cmd, "output": output }))
    }
}