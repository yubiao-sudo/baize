//! 运行环境自检（首次启动引导 + 设置页手动复检）。
//!
//! 检测项分三级：
//!   - required 必需：缺失时白泽核心体验受损（命令执行 / 网络 / OCR / 磁盘），引导层红 ✕ + 修复指引
//!   - optional 增强：仅影响部分 Agent 能力（本地语音 / 回退 OCR / Node / Git / 音频），缺失可跳过
//!   - info 信息：仅展示（管理员权限）
//!
//! 设计要点：
//!   - 每项独立超时，某项卡死不拖垮整体；检测在 spawn_blocking 执行，不阻塞主线程
//!   - 逐项完成后 emit `baize:env-check`，前端引导层实时刷新（单卡片逐项出结论）
//!   - 结果 JSON 落盘 settings("env_report")，后续启动只读缓存秒判断，不重复探测
//!   - 检测到的运行时路径自动索引进 settings（runtime_python / runtime_node /
//!     runtime_git / runtime_tesseract / runtime_kokoro_dir），tts.rs 等优先读配置，
//!     解除 F:\kokoro-tts 硬编码
//!   - onboarding_done 由前端在引导完成/跳过时写（"done" / "skipped"）

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};

/// 防并发：同一时刻只允许一轮检测（引导层 + 设置页可能同时触发）
static RUNNING: AtomicBool = AtomicBool::new(false);

/// 单项检测结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvItem {
    pub id: String,
    /// 展示名
    pub name: String,
    /// required / optional / info
    pub level: String,
    /// ok / warn / missing
    pub status: String,
    #[serde(default)]
    pub version: String,
    /// 厂商 / 来源
    #[serde(default)]
    pub vendor: String,
    /// 检测到的安装路径（自动索引用）
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub detail: String,
    /// 缺失时的影响说明 / 修复指引
    #[serde(default)]
    pub hint: String,
    /// 可一键复制的修复命令（如 winget install …）
    #[serde(default)]
    pub fix_cmd: String,
}

impl EnvItem {
    fn new(id: &str, name: &str, level: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            level: level.into(),
            status: "warn".into(),
            version: String::new(),
            vendor: String::new(),
            path: String::new(),
            detail: String::new(),
            hint: String::new(),
            fix_cmd: String::new(),
        }
    }
}

/// 完整检测报告（落盘 settings("env_report")）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvReport {
    /// 检测完成时间（ms 时间戳）
    pub time: u64,
    pub items: Vec<EnvItem>,
}

/// 启动时的环境状态：缓存报告 + 首次引导标记
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvState {
    pub report: Option<EnvReport>,
    /// "" = 未引导（首次启动）；"done" / "skipped"
    pub onboarding_done: String,
}

// ───────────────────── 子进程探测（带超时，静默无黑窗） ─────────────────────

/// 执行命令返回首个非空输出行（stdout 优先，回退 stderr），超时/失败返回 Err。
/// 例：version_of("git", ["--version"]) → "git version 2.45.0"
fn probe_line(program: &str, args: &[&str], timeout_secs: u64) -> Result<String, String> {
    use std::io::Read;
    let mut child = crate::tools::silent_command(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 {program} 失败: {e}"))?;
    let stdout = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = s.read_to_end(&mut b);
            String::from_utf8_lossy(&b).to_string()
        })
    });
    let stderr = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = s.read_to_end(&mut b);
            String::from_utf8_lossy(&b).to_string()
        })
    });
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let _status = loop {
        if let Ok(Some(st)) = child.try_wait() {
            break st;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{program} 探测超时"));
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    let out = stdout
        .and_then(|h| h.join().ok())
        .unwrap_or_default()
        .trim()
        .to_string();
    let err = stderr
        .and_then(|h| h.join().ok())
        .unwrap_or_default()
        .trim()
        .to_string();
    let line = if out.is_empty() { err } else { out };
    let line = first_line(&line);
    if line.is_empty() {
        return Err(format!("{program} 无输出"));
    }
    Ok(line.to_string())
}

/// 取首个非空行（独立函数避免闭包生命周期问题）
fn first_line(s: &str) -> &str {
    s.lines().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ───────────────────── 各检测项 ─────────────────────

fn detect_powershell() -> EnvItem {
    let mut it = EnvItem::new("powershell", "Windows PowerShell", "required");
    it.vendor = "Microsoft".into();
    match probe_line(
        "powershell",
        &["-NoProfile", "-Command", "$PSVersionTable.PSVersion.ToString()"],
        6,
    ) {
        Ok(v) if v.contains('.') => {
            it.status = "ok".into();
            it.version = v.clone();
            it.detail = format!("PowerShell {v} 可用");
        }
        _ => {
            it.status = "missing".into();
            it.detail = "未检测到 PowerShell".into();
            it.hint = "命令执行类任务不可用。PowerShell 为 Windows 系统组件，请在「启用或关闭 Windows 功能」或 Windows 更新中恢复".into();
        }
    }
    it
}

fn detect_network() -> EnvItem {
    let mut it = EnvItem::new("network", "网络连通", "required");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(4))
        .build();
    match client {
        Ok(c) => {
            let start = Instant::now();
            match c.head("https://www.baidu.com").send() {
                Ok(r) if r.status().is_success() => {
                    it.status = "ok".into();
                    it.detail = format!("外网可达（{}ms）", start.elapsed().as_millis());
                }
                Ok(r) => {
                    it.status = "warn".into();
                    it.detail = format!("外网返回 {}，可能需要代理", r.status());
                    it.hint = "模型 API 多为境外服务，若调用失败请在设置中配置代理".into();
                }
                Err(e) => {
                    it.status = "missing".into();
                    it.detail = "无法访问外网".into();
                    it.hint = format!("检查网络连接或代理设置（{e}）；离线状态下仅本地功能可用");
                }
            }
        }
        Err(e) => {
            it.status = "warn".into();
            it.detail = format!("网络探测器初始化失败: {e}");
        }
    }
    it
}

fn detect_ocr() -> EnvItem {
    let mut it = EnvItem::new("ocr", "Windows OCR 引擎", "required");
    it.vendor = "Microsoft Windows.Media.Ocr".into();
    #[cfg(windows)]
    {
        use windows::Globalization::Language;
        use windows::Media::Ocr::OcrEngine;
        let res = (|| -> Result<String, String> {
            let zh = Language::CreateLanguage(&windows::core::HSTRING::from("zh-CN"))
                .map_err(|e| e.to_string())?;
            if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&zh) {
                let tag = engine
                    .RecognizerLanguage()
                    .ok()
                    .and_then(|l| l.LanguageTag().ok())
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                return Ok(if tag.is_empty() {
                    "中文引擎可用".into()
                } else {
                    format!("中文引擎可用（{tag}）")
                });
            }
            if OcrEngine::TryCreateFromUserProfileLanguages().is_ok() {
                return Ok("用户语言引擎可用（缺中文包，中文识别可能退化）".into());
            }
            Err("系统无可用 OCR 语言包".into())
        })();
        match res {
            Ok(d) => {
                it.status = if d.contains("缺中文包") { "warn" } else { "ok" }.into();
                it.detail = d;
            }
            Err(_) => {
                it.status = "missing".into();
                it.detail = "截图文字识别不可用".into();
                it.hint = "请在 系统设置 → 时间和语言 → 语言和区域 中添加中文语言（勾选可选功能）".into();
            }
        }
    }
    #[cfg(not(windows))]
    {
        it.status = "warn".into();
        it.detail = "非 Windows 平台，Windows OCR 不可用".into();
    }
    it
}

fn detect_disk() -> EnvItem {
    let mut it = EnvItem::new("disk", "磁盘空间", "required");
    // 输出形如 "C:123.4;D:567.8"（盘符:剩余GB）
    let script = "($d=Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3') -ne $null; ($d | ForEach-Object { $_.DeviceID + [string][math]::Round($_.FreeSpace/1GB,1) }) -join ';'";
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    match probe_line("powershell", &["-NoProfile", "-Command", script], 10) {
        Ok(out) => {
            let mut free: Option<f64> = None;
            let mut max_free = 0.0_f64;
            for part in out.split(';') {
                let part = part.trim();
                if part.len() < 2 {
                    continue;
                }
                let (drive, num) = part.split_at(2);
                if let Ok(gb) = num.trim().parse::<f64>() {
                    if drive.eq_ignore_ascii_case(&system_drive) {
                        free = Some(gb);
                    }
                    if gb > max_free {
                        max_free = gb;
                    }
                }
            }
            let free = free.unwrap_or(max_free);
            it.detail = format!("系统盘 {system_drive} 剩余 {free:.1} GB");
            if free >= 10.0 {
                it.status = "ok".into();
            } else if free >= 3.0 {
                it.status = "warn".into();
                it.hint = "系统盘空间偏低：白泽记忆库、截图与日志均在本机落盘，建议清理至 10GB 以上".into();
            } else {
                it.status = "missing".into();
                it.hint = "系统盘空间不足：请先清理磁盘（磁盘清理 / 存储感知），否则数据库写入可能失败".into();
            }
        }
        Err(_) => {
            it.status = "warn".into();
            it.detail = "磁盘检测失败（跳过）".into();
        }
    }
    it
}

fn detect_admin() -> EnvItem {
    let mut it = EnvItem::new("admin", "运行权限", "info");
    it.vendor = "Windows UAC".into();
    match probe_line(
        "powershell",
        &["-NoProfile", "-Command",
          "(New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)"],
        6,
    ) {
        Ok(v) if v.eq_ignore_ascii_case("true") => {
            it.status = "ok".into();
            it.detail = "以管理员身份运行".into();
        }
        _ => {
            it.status = "ok".into();
            it.detail = "普通用户权限（部分系统级安装需提权）".into();
        }
    }
    it
}

/// 运行时探测：返回 (版本, 路径)。PATH 优先，再扫常见安装目录。
fn detect_runtime(
    id: &str,
    name: &str,
    vendor: &str,
    hint: &str,
    fix_cmd: &str,
    program: &str,
    args: &[&str],
    extra_paths: &[String],
) -> EnvItem {
    let mut it = EnvItem::new(id, name, "optional");
    it.vendor = vendor.into();
    it.hint = hint.into();
    it.fix_cmd = fix_cmd.into();
    // 1) PATH 探测（版本输出校验：必须含程序名或版本号特征，防 Windows Store 别名误判）
    if let Ok(line) = probe_line(program, args, 4) {
        let looks_valid = line.to_lowercase().contains(id) || line.starts_with('v') || line.chars().next().is_some_and(|c| c.is_ascii_digit());
        if looks_valid {
            it.status = "ok".into();
            it.version = line;
            if let Ok(path) = probe_line("powershell", &["-NoProfile", "-Command", &format!("(Get-Command {program} -ErrorAction SilentlyContinue).Source")], 5) {
                it.path = path;
            }
            return it;
        }
    }
    // 2) 常见安装路径兜底（PATH 未收录但确实装了）
    for p in extra_paths {
        if std::path::Path::new(p).exists() {
            if let Ok(line) = probe_line(p, args, 4) {
                it.status = "ok".into();
                it.version = line;
                it.path = p.clone();
                it.detail = "已安装（未加入 PATH）".into();
                return it;
            }
        }
    }
    it.status = "missing".into();
    it
}

fn detect_python() -> EnvItem {
    let mut extra: Vec<String> = vec![];
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    if !local.is_empty() {
        for v in ["313", "312", "311", "310"] {
            extra.push(format!("{local}\\Programs\\Python\\Python{v}\\python.exe"));
        }
    }
    let mut it = detect_runtime(
        "python",
        "Python",
        "Python Software Foundation",
        "本地 Kokoro 语音等增强能力依赖 Python；缺失不影响聊天与核心功能",
        "winget install -e --id Python.Python.3.11",
        "python",
        &["--version"],
        &extra,
    );
    // py 启动器兜底（装了 Python 但没进 PATH 的常见形态）
    if it.status == "missing" {
        if let Ok(v) = probe_line("py", &["-3", "--version"], 4) {
            it.status = "ok".into();
            it.version = v;
            if let Ok(path) = probe_line("py", &["-3", "-c", "import sys;print(sys.executable)"], 4) {
                it.path = path;
            }
        }
    }
    it
}

fn detect_kokoro(store: &crate::memory::MemoryStore) -> EnvItem {
    let mut it = EnvItem::new("kokoro", "Kokoro 本地语音", "optional");
    it.vendor = "Kokoro-82M (hexgrad) · 免费离线中文 TTS".into();
    let dir = store
        .get_setting("runtime_kokoro_dir")
        .ok()
        .flatten()
        .unwrap_or_else(|| "F:\\kokoro-tts".into());
    let py = std::path::Path::new(&dir).join("venv").join("Scripts").join("python.exe");
    let server = std::path::Path::new(&dir).join("server.py");
    // 服务是否已在跑（端口 9800 /health）
    let health_up = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1500))
        .build()
        .ok()
        .and_then(|c| c.get("http://127.0.0.1:9800/health").send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    if health_up {
        it.status = "ok".into();
        it.version = "v1.1-zh".into();
        it.path = dir;
        it.detail = "服务运行中（端口 9800）".into();
    } else if py.exists() && server.exists() {
        it.status = "ok".into();
        it.version = "v1.1-zh".into();
        it.path = dir;
        it.detail = "已安装（服务未启动，使用时自动拉起）".into();
    } else {
        it.status = "missing".into();
        it.detail = "未安装".into();
        it.hint = "免费离线中文语音合成（模型约 800MB，依赖 Python venv）。缺失时自动回退云端/浏览器语音，不影响其他功能".into();
    }
    it
}

fn detect_tesseract() -> EnvItem {
    let extra = vec![
        "C:\\Program Files\\Tesseract-OCR\\tesseract.exe".to_string(),
        "C:\\Program Files (x86)\\Tesseract-OCR\\tesseract.exe".to_string(),
    ];
    detect_runtime(
        "tesseract",
        "Tesseract OCR",
        "Google / UB-Mannheim",
        "OCR 二级回退引擎（系统引擎不可用时启用，小字识别更好）",
        "winget install -e --id UB-Mannheim.TesseractOCR",
        "tesseract",
        &["--version"],
        &extra,
    )
}

fn detect_audio() -> EnvItem {
    let mut it = EnvItem::new("audio", "音频设备", "optional");
    let script = "$d=Get-CimInstance Win32_SoundDevice | Where-Object {$_.Status -eq 'OK'} | Select-Object -First 1 -ExpandProperty Name; if($d){$d}else{'NONE'}";
    match probe_line("powershell", &["-NoProfile", "-Command", script], 8) {
        Ok(name) if !name.is_empty() && name != "NONE" => {
            it.status = "ok".into();
            it.vendor = name;
            it.detail = "音频设备正常".into();
        }
        _ => {
            it.status = "missing".into();
            it.detail = "未检测到可用音频设备".into();
            it.hint = "语音朗读与语音对话不可用，请在系统声音设置中检查设备".into();
        }
    }
    it
}

fn detect_node() -> EnvItem {
    detect_runtime(
        "node",
        "Node.js",
        "OpenJS Foundation",
        "仅影响 Agent 代跑 Node/前端项目类任务，白泽自身不依赖",
        "winget install -e --id OpenJS.NodeJS.LTS",
        "node",
        &["--version"],
        &[],
    )
}

fn detect_git() -> EnvItem {
    detect_runtime(
        "git",
        "Git",
        "Git SCM",
        "仅影响 Agent 代跑代码仓库类任务（克隆/提交），白泽自身不依赖",
        "winget install -e --id Git.Git",
        "git",
        &["--version"],
        &[],
    )
}

// ───────────────────── 全量检测 + 自动索引 ─────────────────────

/// 顺序执行全部检测项（每项独立超时），逐项 emit；完成后把运行时路径自动索引进 settings。
fn detect_all_sync(store: &crate::memory::MemoryStore, app: &AppHandle) -> Vec<EnvItem> {
    let mut items: Vec<EnvItem> = Vec::new();
    let mut push = |it: EnvItem| {
        let _ = app.emit("baize:env-check", &it);
        println!("[环境检测] {} → {}", it.name, it.status);
        items.push(it);
    };
    // 必需项优先出结论，增强项随后
    push(detect_powershell());
    push(detect_network());
    push(detect_ocr());
    push(detect_disk());
    push(detect_admin());
    push(detect_python());
    push(detect_kokoro(store));
    push(detect_tesseract());
    push(detect_audio());
    push(detect_node());
    push(detect_git());

    // 自动索引：检测到的路径写入 settings，后续功能免重复探测
    let index = |key: &str, item: &EnvItem| {
        if item.status == "ok" && !item.path.is_empty() {
            let _ = store.set_setting(key, &item.path);
        }
    };
    index("runtime_python", &items[5]);
    index("runtime_tesseract", &items[7]);
    index("runtime_node", &items[9]);
    index("runtime_git", &items[10]);
    // Kokoro 目录单独处理：同步到 tts.rs 进程内缓存（解除硬编码）
    let kokoro = &items[6];
    if kokoro.status == "ok" && !kokoro.path.is_empty() {
        let _ = store.set_setting("runtime_kokoro_dir", &kokoro.path);
        crate::tts::set_kokoro_dir(kokoro.path.clone());
    }
    items
}

// ───────────────────── Tauri 命令 ─────────────────────

/// 全量环境检测（异步：探测在 blocking 线程池执行，逐项 emit 推送前端）。
/// 结果落盘 settings("env_report")。同一时刻仅允许一轮。
#[tauri::command]
pub async fn env_detect_all(
    app: AppHandle,
    state: State<'_, crate::AppState>,
) -> Result<Vec<EnvItem>, String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("环境检测正在进行中".into());
    }
    let store = state.store.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let items = detect_all_sync(&store, &app);
        let report = EnvReport {
            time: now_ms(),
            items: items.clone(),
        };
        if let Ok(j) = serde_json::to_string(&report) {
            let _ = store.set_setting("env_report", &j);
        }
        items
    })
    .await
    .map_err(|e| format!("环境检测任务失败: {e}"));
    RUNNING.store(false, Ordering::SeqCst);
    res
}

/// 读取缓存报告 + 首次引导标记（启动时秒判断，不触发探测）
#[tauri::command]
pub fn env_get_state(state: State<'_, crate::AppState>) -> Result<EnvState, String> {
    let report = state
        .store
        .get_setting("env_report")
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str::<EnvReport>(&j).ok());
    let onboarding_done = state
        .store
        .get_setting("onboarding_done")
        .ok()
        .flatten()
        .unwrap_or_default();
    Ok(EnvState {
        report,
        onboarding_done,
    })
}

/// 写入首次引导标记（"done" = 完成；"skipped" = 跳过）
#[tauri::command]
pub fn env_set_onboarding(state: State<'_, crate::AppState>, done: String) -> Result<(), String> {
    state.store.set_setting("onboarding_done", &done)
}
