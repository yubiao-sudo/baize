//! 软件管家（Software Butler）：帮用户「找软件 / 装软件 / 配置系统」。
//!
//! 设计要点（参考 WorkBuddy 的三层能力，落到白泽本地优先架构）：
//!   1. 环境探测 env_check —— 先摸清系统状态（包管理器、运行时、管理员权限），避免盲目执行；
//!   2. 找软件软件 search / info / list —— 基于系统包管理器（Windows 首选 winget，回退 choco/scoop；
//!      Unix 用 apt/brew/pacman）做真实检索，而非硬编码目录；
//!   3. 装/卸软件 install / uninstall —— 静默安装 + 超时控制，走 HighRisk 人工审批；
//!   4. 配置系统 get / set —— 读系统信息、读写用户级环境变量/PATH、管理开机启动项，走审批。
//!
//! 除 install/uninstall/set 外全部只读；写操作统一 HighRisk，复用现有「三级权限审批」链路。

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::capability::Capability;
use crate::AppState;
use crate::tools::{PermissionClass, Tool};

/// 命令执行结果
struct CmdOut {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

/// 以超时方式执行命令，后台线程读 stdout/stderr 避免输出量大时死锁
fn run(program: &str, args: &[&str], timeout_secs: u64) -> Result<CmdOut, String> {
    let start = std::time::Instant::now();
    let mut child = crate::tools::silent_command(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 {program} 失败: {e}"))?;

    let stdout_reader = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).to_string()
        })
    });
    let stderr_reader = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).to_string()
        })
    });

    let deadline = start + std::time::Duration::from_secs(timeout_secs);
    let status = loop {
        if let Ok(Some(st)) = child.try_wait() {
            break st;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("命令超时（{timeout_secs}s），已终止"));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    Ok(CmdOut {
        stdout: stdout_reader.and_then(|h| h.join().ok()).unwrap_or_default(),
        stderr: stderr_reader.and_then(|h| h.join().ok()).unwrap_or_default(),
        exit_code: status.code().unwrap_or(-1),
    })
}

/// 输出流来源（stdout / stderr），用于流式进度时区分
enum StreamKind {
    Out,
    Err,
}

/// 流式执行命令：边产生输出边回调（on_line 每条输出行、on_heartbeat 无输出超 1s 时），
/// 结束后返回完整 stdout/stderr/exit_code。相比 run()，它不等到命令结束才一次性返回全部输出，
/// 适合 winget 安装这类耗时命令做「实时进度反馈」。
fn run_stream<F, H>(
    program: &str,
    args: &[&str],
    timeout_secs: u64,
    mut on_line: F,
    mut on_heartbeat: H,
) -> Result<CmdOut, String>
where
    F: FnMut(&str),
    H: FnMut(u64),
{
    let start = Instant::now();
    let mut child = crate::tools::silent_command(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 {program} 失败: {e}"))?;

    let (tx, rx) = mpsc::channel::<(StreamKind, String)>();

    if let Some(mut stdout) = child.stdout.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(&mut stdout);
            for line in reader.lines() {
                let Ok(l) = line else { break };
                if tx.send((StreamKind::Out, l)).is_err() {
                    break;
                }
            }
        });
    }
    if let Some(mut stderr) = child.stderr.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(&mut stderr);
            for line in reader.lines() {
                let Ok(l) = line else { break };
                if tx.send((StreamKind::Err, l)).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
    let mut last_emit = Instant::now();
    let deadline = start + std::time::Duration::from_secs(timeout_secs);

    let exit_code = loop {
        match rx.recv_timeout(std::time::Duration::from_millis(300)) {
            Ok((kind, line)) => {
                match kind {
                    StreamKind::Out => {
                        stdout_buf.push_str(&line);
                        stdout_buf.push('\n');
                    }
                    StreamKind::Err => {
                        stderr_buf.push_str(&line);
                        stderr_buf.push('\n');
                    }
                }
                let t = line.trim();
                if !t.is_empty() {
                    on_line(t);
                    last_emit = Instant::now();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if let Ok(Some(st)) = child.try_wait() {
                    break st.code().unwrap_or(-1);
                }
            }
        }

        // 心跳：无新输出超 1s 时，推送「仍在进行中」
        if last_emit.elapsed() >= std::time::Duration::from_secs(1) {
            let elapsed = start.elapsed().as_secs();
            if elapsed > 0 {
                on_heartbeat(elapsed);
                last_emit = Instant::now();
            }
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("命令超时（{timeout_secs}s），已终止"));
        }
        if let Ok(Some(st)) = child.try_wait() {
            // 进程已结束：排空残余输出
            while let Ok((kind, line)) = rx.try_recv() {
                match kind {
                    StreamKind::Out => {
                        stdout_buf.push_str(&line);
                        stdout_buf.push('\n');
                    }
                    StreamKind::Err => {
                        stderr_buf.push_str(&line);
                        stderr_buf.push('\n');
                    }
                }
                let t = line.trim();
                if !t.is_empty() {
                    on_line(t);
                }
            }
            break st.code().unwrap_or(-1);
        }
    };

    Ok(CmdOut {
        stdout: stdout_buf,
        stderr: stderr_buf,
        exit_code,
    })
}

/// 执行 PowerShell 脚本
fn run_pwsh(script: &str, timeout_secs: u64) -> Result<CmdOut, String> {
    // 强制 UTF-8 输出：否则 PowerShell 5.1 会按系统代码页（简体中文 GBK）输出，
    // 中文卷标/软件名成为非 UTF-8 字节，导致 Rust 端读取/JSON 解析失败，
    // 进而磁盘/软件/环境检测返回空，误判“非 C 盘空间不足”并退回 C 盘。
    let wrapped = format!(
        "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;$OutputEncoding=[System.Text.Encoding]::UTF8;{script}"
    );
    run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", &wrapped],
        timeout_secs,
    )
}

/// 命令是否存在（用于探测包管理器）
fn command_exists(cmd: &str) -> bool {
    #[cfg(windows)]
    let out = run("where", &[cmd], 5);
    #[cfg(not(windows))]
    let out = run("sh", &["-c", &format!("command -v {cmd}")], 5);
    out.map(|o| o.exit_code == 0).unwrap_or(false)
}

/// PowerShell 单引号转义（`'` → `''`）
fn psq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 去掉首尾空白，并截断超长输出（保留首部）
fn clean(mut s: String, cap: usize) -> String {
    s = s.trim().to_string();
    if s.chars().count() > cap {
        s = s.chars().take(cap).collect::<String>();
        s.push_str("\n...(已截断)");
    }
    s
}

/// 探测当前系统可用的包管理器
fn detect_package_managers() -> Vec<Value> {
    #[cfg(windows)]
    let candidates: Vec<(&str, &str, &str)> = vec![
        ("winget", "Windows 程序包管理器 (winget)", "winget"),
        ("choco", "Chocolatey (choco)", "choco"),
        ("scoop", "Scoop (scoop)", "scoop"),
    ];
    #[cfg(not(windows))]
    let candidates: Vec<(&str, &str, &str)> = vec![
        ("brew", "Homebrew (brew)", "brew"),
        ("apt", "APT (apt)", "apt"),
        ("pacman", "Pacman (pacman)", "pacman"),
    ];

    candidates
        .into_iter()
        .map(|(id, label, bin)| {
            json!({ "id": id, "label": label, "available": command_exists(bin) })
        })
        .collect()
}

/// 首选包管理器 id（若一个都不可用返回 None）
fn primary_pm() -> Option<&'static str> {
    #[cfg(windows)]
    {
        if command_exists("winget") {
            return Some("winget");
        }
        if command_exists("choco") {
            return Some("choco");
        }
        if command_exists("scoop") {
            return Some("scoop");
        }
        None
    }
    #[cfg(not(windows))]
    {
        if command_exists("brew") {
            return Some("brew");
        }
        if command_exists("apt") {
            return Some("apt");
        }
        if command_exists("pacman") {
            return Some("pacman");
        }
        None
    }
}

/// 解析 winget 的对齐表格输出（按表头列位置切片）
fn parse_table(text: &str, id_header: Option<&str>) -> Vec<Value> {
    let lines: Vec<&str> = text.lines().collect();
    let mut header_idx = None;
    for (i, l) in lines.iter().enumerate() {
        if l.contains("Name") && l.contains("Version") {
            header_idx = Some(i);
            break;
        }
    }
    let Some(hi) = header_idx else { return vec![] };
    let header = lines[hi];

    let name_pos = header.find("Name").unwrap_or(0);
    let version_pos = header.find("Version").unwrap_or(header.len());
    let id_pos = header.find("Id").unwrap_or(version_pos);
    let source_pos = header.find("Source").unwrap_or(header.len());
    // winget 的表格里 Version 与 Source 之间还可能夹着 Match（搜索）或 Available（列表）列，
    // 用它作为 version 的右边界，避免把多余列并进版本号
    let match_pos = header.find("Match").unwrap_or(header.len());
    let avail_pos = header.find("Available").unwrap_or(header.len());
    let version_end = match_pos.min(avail_pos).min(source_pos);

    let col = |pos: usize, end: usize, line: &str| -> String {
        if pos >= line.len() {
            return String::new();
        }
        line[pos..end.min(line.len())].trim().to_string()
    };

    let mut out = Vec::new();
    for l in &lines[hi + 1..] {
        if l.trim().is_empty() || l.trim_start().starts_with('-') {
            continue;
        }
        let version = col(version_pos, version_end, l);
        let source = col(source_pos, l.len(), l);
        let item = match id_header {
            Some(_) => {
                // winget 列序为 Name / Id / Version / Source：Name 列到 Id 列为止
                let name = col(name_pos, id_pos, l);
                let id = col(id_pos, version_pos, l);
                if name.is_empty() || id.is_empty() {
                    continue;
                }
                json!({ "name": name, "id": id, "version": version, "source": source })
            }
            None => {
                let name = col(name_pos, version_pos, l);
                if name.is_empty() {
                    continue;
                }
                json!({ "name": name, "id": name, "version": version, "source": source })
            }
        };
        out.push(item);
    }
    out.truncate(50);
    out
}

/// 执行搜索，返回 { pm, packages, raw }
fn search(query: &str) -> Result<Value, String> {
    let pm = primary_pm().ok_or("未检测到可用的包管理器（Windows 需 winget/choco/scoop；Linux/macOS 需 apt/brew/pacman）")?;

    let out = match pm {
        "winget" => run("winget", &["search", "--query", query, "--accept-source-agreements"], 120)?,
        "choco" => run("choco", &["search", query, "--limit-output"], 120)?,
        "scoop" => run("scoop", &["search", query], 120)?,
        "brew" => run("brew", &["search", query], 120)?,
        "apt" => run("apt", &["search", query], 120)?,
        "pacman" => run("pacman", &["-Ss", query], 120)?,
        _ => return Err(format!("不支持的包管理器: {pm}")),
    };

    let packages: Vec<Value> = match pm {
        "winget" => parse_table(&out.stdout, Some("Id")),
        "choco" => out
            .stdout
            .lines()
            .filter_map(|l| {
                let (name, ver) = l.split_once('|')?;
                if name.trim().is_empty() {
                    return None;
                }
                Some(json!({ "name": name.trim(), "id": name.trim(), "version": ver.trim(), "source": "choco" }))
            })
            .take(50)
            .collect(),
        _ => parse_table(&out.stdout, None),
    };

    Ok(json!({
        "pm": pm,
        "packages": packages,
        "raw": clean(out.stdout, 6000),
    }))
}

fn package_op(op: &str, id: &str, timeout_secs: u64) -> Result<Value, String> {
    let pm = primary_pm().ok_or("未检测到可用的包管理器")?;
    let mut location: Option<String> = None;
    let out = match (pm, op) {
        ("winget", "install") => {
            // 智能选址：检测磁盘空间 + 装机习惯，避开系统盘 C:
            let root = recommend_install_root();
            let loc = root["path"].as_str().unwrap_or("").to_string();
            let mut argv: Vec<String> = vec![
                "install".into(),
                "--id".into(),
                id.to_string(),
                "--silent".into(),
                "--accept-package-agreements".into(),
                "--accept-source-agreements".into(),
            ];
            if !loc.is_empty() {
                argv.push("--location".into());
                argv.push(loc.clone());
            }
            let argv_ref: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
            let r = run("winget", &argv_ref, timeout_secs)?;
            location = Some(loc);
            r
        }
        ("winget", "uninstall") => run("winget", &["uninstall", "--id", id, "--silent"], timeout_secs)?,
        ("winget", "info") => run("winget", &["show", "--id", id, "--accept-source-agreements"], 90)?,
        ("choco", "install") => run("choco", &["install", id, "-y"], timeout_secs)?,
        ("choco", "uninstall") => run("choco", &["uninstall", id, "-y"], timeout_secs)?,
        ("choco", "info") => run("choco", &["info", id], 90)?,
        ("scoop", "install") => run("scoop", &["install", id], timeout_secs)?,
        ("scoop", "uninstall") => run("scoop", &["uninstall", id], timeout_secs)?,
        ("scoop", "info") => run("scoop", &["info", id], 90)?,
        ("brew", "install") => run("brew", &["install", id], timeout_secs)?,
        ("brew", "uninstall") => run("brew", &["uninstall", id], timeout_secs)?,
        ("brew", "info") => run("brew", &["info", id], 90)?,
        ("apt", "install") => run("sudo", &["apt", "install", "-y", id], timeout_secs)?,
        ("apt", "uninstall") => run("sudo", &["apt", "remove", "-y", id], timeout_secs)?,
        ("apt", "info") => run("apt", &["show", id], 90)?,
        ("pacman", "install") => run("sudo", &["pacman", "-S", "--noconfirm", id], timeout_secs)?,
        ("pacman", "uninstall") => run("sudo", &["pacman", "-R", "--noconfirm", id], timeout_secs)?,
        ("pacman", "info") => run("pacman", &["-Si", id], 90)?,
        _ => return Err(format!("不支持的组合: {pm} {op}")),
    };

    let mut res = json!({
        "pm": pm,
        "op": op,
        "id": id,
        "exit_code": out.exit_code,
        "stdout": clean(out.stdout, 6000),
        "stderr": clean(out.stderr, 2000),
    });
    if let Some(loc) = location {
        res["location"] = json!(loc);
    }
    Ok(res)
}

fn list_installed() -> Result<Value, String> {
    // Windows 优先：直接读注册表卸载项，得到「完整已装软件清单」（系统 + 用户、64/32 位视图），
    // 不依赖任何包管理器 —— 这是白泽自带的查看能力；注册表为空才回退包管理器。
    #[cfg(windows)]
    {
        let reg = list_registry();
        if !reg.is_empty() {
            let mut packages = Vec::new();
            for p in reg {
                let name = p["name"].as_str().unwrap_or("").trim().to_string();
                if name.is_empty() {
                    continue;
                }
                packages.push(json!({
                    "name": name,
                    "id": name,
                    "version": p["version"].as_str().unwrap_or("").trim(),
                    "publisher": p["publisher"].as_str().unwrap_or("").trim(),
                    "location": p["location"].as_str().unwrap_or("").trim(),
                    "source": "registry",
                }));
            }
            return Ok(json!({ "pm": "registry", "packages": packages, "raw": "" }));
        }
    }

    let pm = primary_pm().ok_or("未检测到可用的包管理器")?;
    let out = match pm {
        "winget" => run("winget", &["list", "--accept-source-agreements"], 180)?,
        "choco" => run("choco", &["list", "--limit-output"], 180)?,
        "scoop" => run("scoop", &["list"], 180)?,
        "brew" => run("brew", &["list", "--versions"], 180)?,
        "apt" => run("apt", &["list", "--installed"], 180)?,
        "pacman" => run("pacman", &["-Q"], 180)?,
        _ => return Err(format!("不支持的包管理器: {pm}")),
    };
    let packages = if pm == "choco" {
        out.stdout
            .lines()
            .filter_map(|l| {
                let (name, ver) = l.split_once('|')?;
                Some(json!({ "name": name.trim(), "id": name.trim(), "version": ver.trim() }))
            })
            .take(200)
            .collect()
    } else {
        parse_table(&out.stdout, Some("Id"))
    };
    Ok(json!({
        "pm": pm,
        "packages": packages,
        "raw": clean(out.stdout, 6000),
    }))
}

/// 读 Windows 注册表卸载项（HKCU + HKLM，含 32 位 WOW6432Node 视图），返回完整已装软件清单。
#[cfg(windows)]
fn list_registry() -> Vec<Value> {
    let script = r#"
$paths = @(
  'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
)
Get-ItemProperty $paths -ErrorAction SilentlyContinue |
  Where-Object { $_.DisplayName } |
  ForEach-Object { [PSCustomObject]@{
      name=$_.DisplayName;
      version=$_.DisplayVersion;
      publisher=$_.Publisher;
      location=$_.InstallLocation
  } } |
  Sort-Object name -Unique |
  ConvertTo-Json -Compress
"#;
    let v: Value = run_pwsh(script, 30)
        .ok()
        .and_then(|o| serde_json::from_str(&o.stdout).ok())
        .unwrap_or(Value::Array(vec![]));
    match v {
        Value::Array(a) => a,
        Value::Object(_) => vec![v],
        _ => vec![],
    }
}

#[cfg(not(windows))]
fn list_registry() -> Vec<Value> {
    vec![]
}

fn is_admin() -> bool {
    #[cfg(windows)]
    {
        run_pwsh(
            "(New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
            5,
        )
        .map(|o| o.stdout.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        run("id", &["-u"], 5)
            .map(|o| o.stdout.trim() == "0")
            .unwrap_or(false)
    }
}

fn version_of(program: &str, args: &[&str]) -> Option<String> {
    run(program, args, 8)
        .ok()
        .and_then(|o| {
            let mut s = o.stdout.trim().to_string();
            if s.is_empty() {
                s = o.stderr.trim().to_string();
            }
            s.lines().next().map(|l| l.trim().to_string())
        })
        .filter(|s| !s.is_empty())
}

/// 检测固定磁盘（Windows 通过 Win32_LogicalDisk；Unix 返回空，安装位置由包管理器决定）
#[cfg(windows)]
fn detect_disks() -> Vec<Value> {
    let script = r#"Get-CimInstance Win32_LogicalDisk -Filter "DriveType=3" | ForEach-Object { [PSCustomObject]@{ drive=$_.DeviceID; label=$_.VolumeName; total_gb=[math]::Round($_.Size/1GB,1); free_gb=[math]::Round($_.FreeSpace/1GB,1) } } | ConvertTo-Json -Compress"#;
    let v: Value = run_pwsh(script, 10)
        .ok()
        .and_then(|o| serde_json::from_str(&o.stdout).ok())
        .unwrap_or(Value::Array(vec![]));
    match v {
        Value::Array(a) => a,
        Value::Object(_) => vec![v],
        _ => vec![],
    }
}

#[cfg(not(windows))]
fn detect_disks() -> Vec<Value> {
    vec![]
}

/// 装机习惯评分：统计某盘上「已存在的程序目录」数量，数量越多说明用户习惯装在该盘
#[cfg(windows)]
fn habit_score(drive: &str) -> usize {
    const DIRS: &[&str] = &[
        "Program Files",
        "Program Files (x86)",
        "Programs",
        "Apps",
        "Software",
        "soft",
        "应用",
        "软件",
    ];
    let mut score = 0usize;
    for sub in DIRS {
        let p = format!("{drive}\\{sub}");
        if let Ok(entries) = std::fs::read_dir(&p) {
            score += entries.flatten().count().min(20);
        }
    }
    score.min(100)
}

/// 推荐安装位置：优先「非系统盘 + 有装机习惯 + 空间充足」，否则「非系统盘 + 空间最大」，最后退回系统盘
pub fn recommend_install_root() -> Value {
    #[cfg(windows)]
    {
        let disks = detect_disks();
        let mut cands: Vec<(String, f64, usize)> = vec![];
        let mut fallback_drive = "C:".to_string();
        let mut fallback_free = 0.0_f64;
        for d in &disks {
            let drive = d["drive"].as_str().unwrap_or("").to_string();
            let free = d["free_gb"].as_f64().unwrap_or(0.0);
            if drive.is_empty() {
                continue;
            }
            if drive.eq_ignore_ascii_case("C:") {
                fallback_drive = drive;
                fallback_free = free;
                continue;
            }
            let habit = habit_score(&drive);
            cands.push((drive, free, habit));
        }
        // 装机习惯优先，其次剩余空间
        cands.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        if let Some((drive, free, habit)) = cands.iter().find(|(_, f, _)| *f >= 20.0) {
            let drive = drive.clone();
            let free = *free;
            let habit = *habit;
            let reason = if habit > 0 {
                format!("检测到 {drive} 已存放约 {habit} 个程序目录（你的装机习惯），且剩余 {free:.1} GB 空间充足")
            } else {
                format!("{drive} 剩余 {free:.1} GB 空间最充足，避开系统盘 C:")
            };
            return json!({ "drive": drive, "path": format!("{drive}\\Program Files"), "reason": reason, "free_gb": free });
        }
        // 兜底：无非 C 盘达到 20GB —— 在全部固定盘里选剩余空间最大的，而非无条件退回 C 盘
        for (drive, free, _) in &cands {
            if *free > fallback_free {
                fallback_drive = drive.clone();
                fallback_free = *free;
            }
        }
        json!({
            "drive": fallback_drive,
            "path": format!("{fallback_drive}\\Program Files"),
            "reason": format!("非系统盘剩余空间均不足 20GB，选择剩余最大的 {fallback_drive}（{fallback_free:.1} GB）"),
            "free_gb": fallback_free,
        })
    }
    #[cfg(not(windows))]
    {
        json!({ "drive": "", "path": "/usr/local", "reason": "Unix 系统安装位置由包管理器决定", "free_gb": 0.0 })
    }
}

/// 安装预览：给安装审批卡准备的「富信息」（目标位置 + 推荐理由 + 软件名）
pub fn install_preview(args: &Value) -> Value {
    let id = args["id"].as_str().unwrap_or("").to_string();
    let name = args["name"].as_str().unwrap_or("").to_string();
    let name = if name.is_empty() { default_name_from_id(&id) } else { name };
    let root = recommend_install_root();
    json!({
        "id": id,
        "name": name,
        "target": root["path"].as_str().unwrap_or("").to_string(),
        "drive": root["drive"].as_str().unwrap_or("").to_string(),
        "reason": root["reason"].as_str().unwrap_or("").to_string(),
        "free_gb": root["free_gb"].as_f64().unwrap_or(0.0),
    })
}

/// 安装前抓取软件元数据（厂商 / 版本 / 官网），来自 winget show；其余包管理器或失败时返回空对象。
fn install_meta(id: &str) -> Value {
    let pm = primary_pm().unwrap_or("");
    if pm != "winget" {
        return json!({});
    }
    match run("winget", &["show", "--id", id, "--accept-source-agreements"], 20) {
        Ok(out) => parse_winget_show(&out.stdout),
        Err(_) => json!({}),
    }
}

/// 解析 winget show 输出里的 Publisher / Version / Homepage 三行（key 恒为英文，与系统语言无关）
fn parse_winget_show(text: &str) -> Value {
    let mut publisher = String::new();
    let mut version = String::new();
    let mut homepage = String::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Publisher:") {
            publisher = v.trim().to_string();
        } else if let Some(v) = t.strip_prefix("Version:") {
            version = v.trim().to_string();
        } else if let Some(v) = t.strip_prefix("Homepage:") {
            homepage = v.trim().to_string();
        }
    }
    json!({
        "publisher": publisher,
        "version": version,
        "homepage": homepage,
    })
}

/// 从包 id 推导展示名（如 Microsoft.VisualStudioCode → VisualStudioCode）
fn default_name_from_id(id: &str) -> String {
    let seg = id.rsplit('.').next().unwrap_or(id);
    if seg.is_empty() {
        id.to_string()
    } else {
        seg.to_string()
    }
}

// ───────────────────── 工具 1：环境探测 ─────────────────────
pub struct EnvCheckTool;

impl Tool for EnvCheckTool {
    fn name(&self) -> &str {
        "env_check"
    }
    fn description(&self) -> &str {
        "探测当前系统环境：操作系统、可用的包管理器（winget/choco/scoop/apt/brew/pacman）、常用运行时（Node/Git/Python/.NET）版本、是否管理员权限。装软件或配置系统前先调用它了解现状"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let runtimes = json!({
            "node": version_of("node", &["--version"]),
            "git": version_of("git", &["--version"]),
            "python": version_of("python", &["--version"]),
            "dotnet": version_of("dotnet", &["--version"]),
        });
        Ok(json!({
            "os": std::env::consts::OS,
            "is_admin": is_admin(),
            "package_managers": detect_package_managers(),
            "runtimes": runtimes,
        }))
    }
}

// ───────────────────── 工具 2：搜索软件 ─────────────────────
pub struct SoftwareSearchTool;

impl Tool for SoftwareSearchTool {
    fn name(&self) -> &str {
        "software_search"
    }
    fn description(&self) -> &str {
        "在系统包管理器（Windows 优先 winget）中搜索软件，返回候选包（id/名称/版本/来源）与原始输出。找软件用这个工具"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "软件名或关键词，如 vscode、chrome、python" }
            },
            "required": ["query"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let query = args["query"].as_str().ok_or("缺少参数 query")?;
        search(query)
    }
}

// ───────────────────── 工具 3：软件详情 ─────────────────────
pub struct SoftwareInfoTool;

impl Tool for SoftwareInfoTool {
    fn name(&self) -> &str {
        "software_info"
    }
    fn description(&self) -> &str {
        "查看某个软件包的详情（版本、来源、说明等）。参数 id 来自 software_search 的结果"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "包 id，如 Microsoft.VisualStudioCode" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        package_op("info", id, 90)
    }
}

// ───────────────────── 工具 4：已装软件列表 ─────────────────────
pub struct SoftwareListTool;

impl Tool for SoftwareListTool {
    fn name(&self) -> &str {
        "software_list"
    }
    fn description(&self) -> &str {
        "列出本机已安装的软件（Windows 直接读注册表卸载项得到完整清单，不依赖包管理器；Unix 回退包管理器）。用于查看本机装了什么、判断某软件是否已存在"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        list_installed()
    }
}

// ───────────────────── 工具 4.5：磁盘与装机习惯 ─────────────────────
pub struct DiskInfoTool;

impl Tool for DiskInfoTool {
    fn name(&self) -> &str {
        "disk_info"
    }
    fn description(&self) -> &str {
        "检测各磁盘可用空间与用户装机习惯，给出推荐安装盘符/目录（自动避开系统盘 C:）。装软件前先调用它确定要装到哪个盘"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        Ok(json!({
            "os": std::env::consts::OS,
            "disks": detect_disks(),
            "install_root": recommend_install_root(),
        }))
    }
}

// ───────────────────── 工具 5：安装软件 ─────────────────────
pub struct SoftwareInstallTool {
    app: AppHandle,
    capability: Arc<dyn Capability>,
}

impl SoftwareInstallTool {
    pub fn new(app: AppHandle, capability: Arc<dyn Capability>) -> Self {
        Self { app, capability }
    }

    /// 向前端推送安装进度：发「thought（tool_progress）」事件，走执行流实时渲染进度条。
    /// meta 携带厂商/版本/官网，供进度条展示应用头像与信息。
    fn emit_progress(
        &self,
        _id: &str,
        name: &str,
        meta: &Value,
        percent: f64,
        phase: &str,
        message: &str,
    ) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let payload = json!({
            "ts": ts,
            "kind": "tool_progress",
            "label": format!("安装 · {name}"),
            "detail": message,
            "progress": percent,
            "phase": phase,
            "vendor": meta["publisher"].as_str().unwrap_or(""),
            "version": meta["version"].as_str().unwrap_or(""),
            "homepage": meta["homepage"].as_str().unwrap_or(""),
        });
        // 实时推送进度条到前端执行流
        let _ = self.app.emit("thought", payload.clone());
        // 同步固化到执行流日志，安装结束后「总结回复」仍能回看进度条与头像/厂商/版本
        self.app.state::<AppState>().log_thought_full(payload);
    }

    /// 流式安装：选址 → 启动包管理器 → 边输出边推送进度 → 收尾。
    fn run_install(&self, id: &str, name: &str, timeout_secs: u64) -> Result<Value, String> {
        let pm = primary_pm().ok_or("未检测到可用的包管理器")?;
        // 抓取元数据（厂商/版本/官网），随进度事件带到前端展示头像与信息
        let meta = install_meta(id);
        let mut location: Option<String> = None;
        let program: &str;
        let argv: Vec<String>;

        self.emit_progress(id, name, &meta, 5.0, "check", "正在检测磁盘空间与装机习惯…");
        match pm {
            "winget" => {
                program = "winget";
                let root = recommend_install_root();
                let loc = root["path"].as_str().unwrap_or("").to_string();
                let mut v: Vec<String> = vec![
                    "install".into(),
                    "--id".into(),
                    id.to_string(),
                    "--silent".into(),
                    "--accept-package-agreements".into(),
                    "--accept-source-agreements".into(),
                ];
                if !loc.is_empty() {
                    v.push("--location".into());
                    v.push(loc.clone());
                }
                location = Some(loc);
                self.emit_progress(
                    id,
                    name,
                    &meta,
                    12.0,
                    "locate",
                    &format!("将安装到 {}", location.as_deref().unwrap_or("系统默认目录")),
                );
                argv = v;
            }
            "choco" => {
                program = "choco";
                argv = vec!["install".into(), id.to_string(), "-y".into()];
            }
            "scoop" => {
                program = "scoop";
                argv = vec!["install".into(), id.to_string()];
            }
            "brew" => {
                program = "brew";
                argv = vec!["install".into(), id.to_string()];
            }
            "apt" => {
                program = "sudo";
                argv = vec!["apt".into(), "install".into(), "-y".into(), id.to_string()];
            }
            "pacman" => {
                program = "sudo";
                argv = vec!["pacman".into(), "-S".into(), "--noconfirm".into(), id.to_string()];
            }
            _ => return Err(format!("不支持的包管理器: {pm}")),
        }

        let argv_ref: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let start = Instant::now();
        self.emit_progress(id, name, &meta, 15.0, "installing", &format!("已启动 {pm}，开始安装…"));

        // 进度估算：随耗时线性爬升，封顶 92%，完成/失败时收敛到 100%
        let frac_of = |elapsed: f64| -> f64 { (elapsed / timeout_secs as f64).min(0.95) };
        let out = run_stream(
            program,
            &argv_ref,
            timeout_secs,
            |line| {
                let pct = (15.0 + frac_of(start.elapsed().as_secs_f64()) * 75.0).min(92.0);
                self.emit_progress(id, name, &meta, pct, "installing", line);
            },
            |elapsed| {
                let pct = (15.0 + frac_of(elapsed as f64) * 75.0).min(92.0);
                // 边安装边轮询弹窗：安装器弹出「确认/同意/下一步」等对话框时自动点击推进，
                // 避免卡在弹窗上等超时。白泽自带鼠标点击能力，无需写脚本/提权/改注册表。
                match crate::popup::confirm_dialogs(self.capability.as_ref()) {
                    v if v["clicked"].as_bool() == Some(true) => {
                        self.emit_progress(
                            id,
                            name,
                            &meta,
                            pct,
                            "installing",
                            &format!("已自动处理安装弹窗（{}）", v["label"].as_str().unwrap_or("确认")),
                        );
                    }
                    _ => {}
                }
                self.emit_progress(
                    id,
                    name,
                    &meta,
                    pct,
                    "installing",
                    &format!("安装进行中 · 已耗时 {elapsed}s"),
                );
            },
        )?;

        if out.exit_code == 0 {
            self.emit_progress(id, name, &meta, 100.0, "done", "安装完成");
        } else {
            self.emit_progress(id, name, &meta, 100.0, "failed", &format!("安装退出码 {}", out.exit_code));
        }

        let mut res = json!({
            "pm": pm,
            "op": "install",
            "id": id,
            "exit_code": out.exit_code,
            "stdout": clean(out.stdout, 6000),
            "stderr": clean(out.stderr, 2000),
        });
        if let Some(loc) = location {
            res["location"] = json!(loc);
        }
        Ok(res)
    }
}

impl Tool for SoftwareInstallTool {
    fn name(&self) -> &str {
        "software_install"
    }
    fn description(&self) -> &str {
        "通过包管理器安装软件（静默安装，可选超时，实时反馈进度）。自动检测磁盘空间与装机习惯，装到合适盘符（避开系统盘 C:）。id 来自 software_search 结果，name 填软件显示名。属于高危操作，会请求授权"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "包 id，如 Microsoft.VisualStudioCode" },
                "name": { "type": "string", "description": "软件显示名，如 Visual Studio Code" },
                "timeout_secs": { "type": "integer", "description": "超时秒数，默认 600，最大 3600" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        let name = args["name"].as_str().unwrap_or("").trim().to_string();
        let name = if name.is_empty() { default_name_from_id(id) } else { name };
        let timeout = args["timeout_secs"].as_u64().unwrap_or(600).clamp(30, 3600);
        self.run_install(id, &name, timeout)
    }
}

// ───────────────────── 工具 6：卸载软件 ─────────────────────
pub struct SoftwareUninstallTool {
    capability: Arc<dyn Capability>,
}

impl SoftwareUninstallTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }

    /// 流式卸载：启动包管理器 → 边输出边轮询弹窗（确认卸载/是/确定/下一步）→ 收尾。
    /// 部分软件卸载器会连弹多个确认弹窗；这里在命令运行期间反复调用 confirm_dialogs
    /// 自动点击推进，避免卡屏超时。无需脚本/提权/改注册表，直接用内置鼠标点击能力。
    fn run_uninstall(&self, id: &str, timeout_secs: u64) -> Result<Value, String> {
        let pm = primary_pm().ok_or("未检测到可用的包管理器")?;
        let program: &str;
        let argv: Vec<String>;
        match pm {
            "winget" => {
                program = "winget";
                argv = vec!["uninstall".into(), "--id".into(), id.to_string(), "--silent".into()];
            }
            "choco" => {
                program = "choco";
                argv = vec!["uninstall".into(), id.to_string(), "-y".into()];
            }
            "scoop" => {
                program = "scoop";
                argv = vec!["uninstall".into(), id.to_string()];
            }
            "brew" => {
                program = "brew";
                argv = vec!["uninstall".into(), id.to_string()];
            }
            "apt" => {
                program = "sudo";
                argv = vec!["apt".into(), "remove".into(), "-y".into(), id.to_string()];
            }
            "pacman" => {
                program = "sudo";
                argv = vec!["pacman".into(), "-R".into(), "--noconfirm".into(), id.to_string()];
            }
            _ => return Err(format!("不支持的包管理器: {pm}")),
        }

        let argv_ref: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        // 轮询弹窗回调：无输出心跳期（每 ~1s）自动点卸载确认弹窗
        let capability = self.capability.clone();
        let mut clicked_labels: Vec<String> = Vec::new();
        let out = run_stream(
            program,
            &argv_ref,
            timeout_secs,
            |_line| {},
            |_elapsed| {
                let v = crate::popup::confirm_dialogs(capability.as_ref());
                if v["clicked"].as_bool() == Some(true) {
                    let label = v["label"].as_str().unwrap_or("确认").to_string();
                    if !clicked_labels.contains(&label) {
                        clicked_labels.push(label);
                    }
                }
            },
        )?;

        let mut res = json!({
            "pm": pm,
            "op": "uninstall",
            "id": id,
            "exit_code": out.exit_code,
            "stdout": clean(out.stdout, 6000),
            "stderr": clean(out.stderr, 2000),
        });
        if !clicked_labels.is_empty() {
            res["auto_confirmed"] = json!(clicked_labels);
        }
        Ok(res)
    }
}

impl Tool for SoftwareUninstallTool {
    fn name(&self) -> &str {
        "software_uninstall"
    }
    fn description(&self) -> &str {
        "通过包管理器卸载软件。卸载过程中若弹出多个「确认卸载/是/确定/下一步」等确认弹窗，会自动逐个点击推进（内置鼠标点击能力，无需额外脚本）。id 来自 software_list 或 software_search。属于高危操作，会请求授权"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "包 id" },
                "timeout_secs": { "type": "integer", "description": "超时秒数，默认 600" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        let timeout = args["timeout_secs"].as_u64().unwrap_or(600).clamp(30, 3600);
        self.run_uninstall(id, timeout)
    }
}

// ───────────────────── 工具 7：读系统配置 ─────────────────────
pub struct SystemGetTool;

impl Tool for SystemGetTool {
    fn name(&self) -> &str {
        "system_get"
    }
    fn description(&self) -> &str {
        "读取系统配置：操作系统版本、用户与系统的环境变量、用户与系统 PATH 条目、用户与系统的开机启动项。配置系统前先用它了解现状"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        #[cfg(windows)]
        let os_version = run_pwsh("[System.Environment]::OSVersion.VersionString", 5)
            .map(|o| o.stdout.trim().to_string())
            .unwrap_or_default();
        #[cfg(not(windows))]
        let os_version = version_of("uname", &["-r"]).unwrap_or_default();

        #[cfg(windows)]
        let (env, machine_env, path, machine_path, startup, machine_startup) = {
            let read_env = |scope: &str| -> Value {
                run_pwsh(
                    &format!("[Environment]::GetEnvironmentVariables('{scope}') | ConvertTo-Json -Compress"),
                    10,
                )
                .ok()
                .and_then(|o| serde_json::from_str::<Value>(&o.stdout).ok())
                .unwrap_or_else(|| json!({}))
            };
            let read_path = |scope: &str| -> Vec<String> {
                let raw = run_pwsh(
                    &format!("[Environment]::GetEnvironmentVariable('Path','{scope}')"),
                    5,
                )
                .map(|o| o.stdout.trim().to_string())
                .unwrap_or_default();
                std::env::split_paths(&raw)
                    .map(|p| p.to_string_lossy().to_string())
                    .collect()
            };
            let read_run = |hive: &str| -> Value {
                run_pwsh(
                    &format!("Get-ItemProperty '{hive}:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' -ErrorAction SilentlyContinue | Select-Object * -ExcludeProperty PS* | ConvertTo-Json -Compress"),
                    10,
                )
                .ok()
                .and_then(|o| serde_json::from_str::<Value>(&o.stdout).ok())
                .unwrap_or_else(|| json!({}))
            };
            (
                read_env("User"),
                read_env("Machine"),
                read_path("User"),
                read_path("Machine"),
                read_run("HKCU"),
                read_run("HKLM"),
            )
        };
        #[cfg(not(windows))]
        let (env, machine_env, path, machine_path, startup, machine_startup) = {
            let env: Value = std::env::vars()
                .map(|(k, v)| (k, json!(v)))
                .collect::<serde_json::Map<_, _>>()
                .into();
            let path: Vec<String> = std::env::var("PATH")
                .map(|p| std::env::split_paths(&p).map(|x| x.to_string_lossy().to_string()).collect())
                .unwrap_or_default();
            (env, json!({}), path, vec![], json!({}), json!({}))
        };

        Ok(json!({
            "os": std::env::consts::OS,
            "os_version": os_version,
            "env": env,
            "machine_env": machine_env,
            "path": path,
            "machine_path": machine_path,
            "startup": startup,
            "machine_startup": machine_startup,
        }))
    }
}

// ───────────────────── 工具 8：配置系统 ─────────────────────
pub struct SystemSetTool;

impl Tool for SystemSetTool {
    fn name(&self) -> &str {
        "system_set"
    }
    fn description(&self) -> &str {
        "配置系统：设置/删除用户级环境变量、向用户 PATH 追加/移除目录、添加/删除开机启动项。action 选 env_set/env_unset/path_add/path_remove/startup_add/startup_remove。属于高危操作，会请求授权"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["env_set", "env_unset", "path_add", "path_remove", "startup_add", "startup_remove"], "description": "要执行的系统配置动作" },
                "name": { "type": "string", "description": "环境变量名（env_*）或启动项名称（startup_*）" },
                "value": { "type": "string", "description": "env_set 的值 / path_add 的目录 / startup_add 的命令" }
            },
            "required": ["action"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let action = args["action"].as_str().ok_or("缺少参数 action")?;
        let name = args["name"].as_str().unwrap_or("");
        let value = args["value"].as_str().unwrap_or("");

        let script = match action {
            "env_set" => {
                if name.is_empty() || value.is_empty() {
                    return Err("env_set 需要 name 与 value".into());
                }
                format!("[Environment]::SetEnvironmentVariable({}, {}, 'User'); 'ok'", psq(name), psq(value))
            }
            "env_unset" => {
                if name.is_empty() {
                    return Err("env_unset 需要 name".into());
                }
                format!("[Environment]::SetEnvironmentVariable({}, $null, 'User'); 'ok'", psq(name))
            }
            "path_add" => {
                if value.is_empty() {
                    return Err("path_add 需要 value（目录路径）".into());
                }
                format!(
                    "$p=[Environment]::GetEnvironmentVariable('Path','User'); if(@($p -split ';') -notcontains {}){{[Environment]::SetEnvironmentVariable('Path', ($p.TrimEnd(';')+';'+{}),'User')}}; 'ok'",
                    psq(value), psq(value)
                )
            }
            "path_remove" => {
                if value.is_empty() {
                    return Err("path_remove 需要 value（目录路径）".into());
                }
                format!(
                    "$p=[Environment]::GetEnvironmentVariable('Path','User'); $e=@($p -split ';') | Where-Object {{ $_ -and ($_ -ne {}) }}; [Environment]::SetEnvironmentVariable('Path', ($e -join ';'),'User'); 'ok'",
                    psq(value)
                )
            }
            "startup_add" => {
                if name.is_empty() || value.is_empty() {
                    return Err("startup_add 需要 name 与 value（命令）".into());
                }
                format!(
                    "New-ItemProperty -Path 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' -Name {} -Value {} -PropertyType String -Force | Out-Null; 'ok'",
                    psq(name), psq(value)
                )
            }
            "startup_remove" => {
                if name.is_empty() {
                    return Err("startup_remove 需要 name".into());
                }
                format!(
                    "Remove-ItemProperty -Path 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' -Name {} -ErrorAction SilentlyContinue; 'ok'",
                    psq(name)
                )
            }
            _ => return Err(format!("不支持的动作: {action}")),
        };

        #[cfg(windows)]
        let out = run_pwsh(&script, 60)?;
        #[cfg(not(windows))]
        let out = {
            // 非 Windows：环境变量用进程级实现，启动项不支持
            let _ = script;
            return Err("system_set 目前仅在 Windows 上支持环境变量与启动项配置".into());
        };

        Ok(json!({
            "action": action,
            "ok": out.exit_code == 0,
            "stdout": clean(out.stdout, 2000),
            "stderr": clean(out.stderr, 2000),
        }))
    }
}