use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindowBuilder,
};

use crate::tools::{PermissionClass, Tool};

/// 桌面步骤弹幕的历史记录（供浮窗加载时补齐，避免漏掉首条弹幕）
static STEP_LOG: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn step_log() -> &'static Mutex<Vec<String>> {
    STEP_LOG.get_or_init(|| Mutex::new(Vec::new()))
}

/// 推出一条步骤：写入历史记录并广播 `step-push` 事件（供浮窗弹幕滚动）。
pub fn push_step(app: &AppHandle, text: &str) {
    let t = text.trim().to_string();
    if t.is_empty() {
        return;
    }
    step_log().lock().unwrap().push(t.clone());
    let _ = app.emit("step-push", json!({ "text": t }));
}

/// 清空步骤历史（GUI 任务结束 / 解除接管时调用，避免下次接管回放旧弹幕）。
pub fn clear_step_log() {
    step_log().lock().unwrap().clear();
}

/// 原生网页窗口编号（每次打开新网页递增，形成独立窗口标签）
static WEBVIEW_COUNTER: AtomicU64 = AtomicU64::new(0);

const SIDE_WIDTH: f64 = 420.0;

/// 确保浏览器窗口存在并显示（按需调出，定位在主窗口左侧，不挤压主窗口）
pub fn ensure_browser_window(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = handle.get_webview_window("browser") {
            let _ = win.show();
            let _ = win.set_focus();
            return;
        }
        let (x, y, h) = side_geometry(&handle, true);
        match WebviewWindowBuilder::new(
            &handle,
            "browser",
            WebviewUrl::App("index.html#/browser".into()),
        )
        .title("白泽 · 浏览器")
        .inner_size(SIDE_WIDTH, h.max(480.0))
        .position(x, y) // 创建时直接定位，避免「先出现在默认位置再跳」的闪烁
        .build()
        {
            Ok(w) => {
                let _ = w.set_focus();
            }
            Err(e) => eprintln!("[窗口] 创建浏览器窗口失败: {e}"),
        }
    });
}

/// 确保文档窗口存在并显示（按需调出，定位在主窗口右侧，不挤压主窗口）
pub fn ensure_markdown_window(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = handle.get_webview_window("markdown") {
            let _ = win.show();
            let _ = win.set_focus();
            return;
        }
        let (x, y, h) = side_geometry(&handle, false);
        match WebviewWindowBuilder::new(
            &handle,
            "markdown",
            WebviewUrl::App("index.html#/markdown".into()),
        )
        .title("白泽 · 文档")
        .inner_size(SIDE_WIDTH, h.max(480.0))
        .position(x, y)
        .build()
        {
            Ok(w) => {
                let _ = w.set_focus();
            }
            Err(e) => eprintln!("[窗口] 创建文档窗口失败: {e}"),
        }
    });
}

/// 确保终端窗口存在并显示（按需调出），关闭窗口时回收终端会话
pub fn ensure_terminal_window(app: &AppHandle, terminal: Arc<crate::terminal::TerminalState>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = handle.get_webview_window("terminal") {
            let _ = win.show();
            let _ = win.set_focus();
            return;
        }
        match WebviewWindowBuilder::new(
            &handle,
            "terminal",
            WebviewUrl::App("index.html#/terminal".into()),
        )
        .title("白泽 · 终端")
        .inner_size(760.0, 480.0)
        .on_page_load(|_win, payload| {
            eprintln!(
                "[终端] 页面加载事件: {:?} url={}",
                payload.event(),
                payload.url()
            );
        })
        .build()
        {
            Ok(w) => {
                eprintln!(
                    "[终端] 窗口创建成功，url={:?}",
                    w.url().map(|u| u.to_string())
                );
                let _ = w.set_focus();
                let t = terminal.clone();
                w.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { .. } = event {
                        eprintln!("[终端] 收到关闭事件，回收会话");
                        t.close();
                    }
                });
            }
            Err(e) => eprintln!("[窗口] 创建终端窗口失败: {e}"),
        }
    });
}

/// 打开一个外部网页（原生 WebView 窗口，规避 iframe 内嵌的 X-Frame-Options 限制）。
/// 每次导航新建一个独立网页窗口（标签递增，避免复用窗口需要 navigate 的复杂状态）。
pub fn open_external_browser(app: &AppHandle, url: &str) {
    let handle = app.clone();
    let url = url.to_string();
    let _ = app.run_on_main_thread(move || {
        let n = WEBVIEW_COUNTER.fetch_add(1, Ordering::SeqCst);
        let label = format!("webview-external-{n}");
        let parsed = url
            .parse()
            .unwrap_or_else(|_| "https://example.com".parse().unwrap());
        match WebviewWindowBuilder::new(&handle, label, WebviewUrl::External(parsed))
            .title("白泽 · 网页")
            .inner_size(1100.0, 760.0)
            .build()
        {
            Ok(w) => {
                let _ = w.set_focus();
            }
            Err(e) => eprintln!("[窗口] 创建网页窗口失败: {e}"),
        }
    });
}

/// 调整浏览器 / 文档窗口大小（像素，主线程执行）
pub fn resize_window(app: &AppHandle, target: &str, width: f64, height: f64) {
    let handle = app.clone();
    let label = target.to_string();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = handle.get_webview_window(&label) {
            let _ = win.set_size(LogicalSize::new(width, height));
        }
    });
}

/// 计算侧窗位置：left=true 在主窗口左侧，否则在右侧
fn side_geometry(app: &AppHandle, left: bool) -> (f64, f64, f64) {
    if let Some((mx, my, mw, mh)) = main_geometry(app) {
        let x = if left { (mx - SIDE_WIDTH).max(0.0) } else { mx + mw };
        return (x, my, mh.max(480.0));
    }
    (0.0, 0.0, 720.0)
}

/// 主窗口几何信息（逻辑坐标）
fn main_geometry(app: &AppHandle) -> Option<(f64, f64, f64, f64)> {
    let main = app.get_webview_window("main")?;
    let pos = main.outer_position().ok()?;
    let size = main.outer_size().ok()?;
    let scale = main.scale_factor().ok()?;
    Some((
        pos.x as f64 / scale,
        pos.y as f64 / scale,
        size.width as f64 / scale,
        size.height as f64 / scale,
    ))
}

// ───────────────────────── resize_window 工具 ─────────────────────────

/// 调整侧窗尺寸工具（白泽控制两个组件的宽高）
pub struct ResizeWindowTool {
    app: AppHandle,
}

impl ResizeWindowTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for ResizeWindowTool {
    fn name(&self) -> &str {
        "resize_window"
    }
    fn description(&self) -> &str {
        "调整内置浏览器或文档窗口的宽度和高度（像素）。用于让白泽按需改变两个组件的大小。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "enum": ["browser", "markdown"],
                    "description": "要调整的窗口：browser=浏览器窗口，markdown=文档窗口"
                },
                "width": { "type": "number", "description": "窗口宽度（像素）" },
                "height": { "type": "number", "description": "窗口高度（像素）" }
            },
            "required": ["target", "width", "height"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let target = args["target"].as_str().ok_or("缺少参数 target")?;
        let width = args["width"].as_f64().ok_or("缺少参数 width")?;
        let height = args["height"].as_f64().ok_or("缺少参数 height")?;
        if target != "browser" && target != "markdown" {
            return Err("target 必须是 browser 或 markdown".to_string());
        }
        if !(200.0..=3000.0).contains(&width) || !(200.0..=3000.0).contains(&height) {
            return Err("宽高需在 200~3000 像素之间".to_string());
        }
        resize_window(&self.app, target, width, height);
        Ok(json!({ "ok": true, "target": target, "width": width, "height": height }))
    }
}

// ───────────────────────── 桌面步骤弹幕浮窗 ─────────────────────────

/// 计算顶部弹幕浮窗几何（逻辑坐标）：横跨主显示器顶部、占一整条细带
fn top_bar_geometry(app: &AppHandle) -> (f64, f64, f64, f64) {
    // 主窗口在接管期间是最小化态，current_monitor() 可能返回 None，
    // 优先用主显示器（弹幕条固定贴主屏顶部）
    let mon = app.primary_monitor().ok().flatten();
    if let Some(mon) = mon {
        let size = mon.size();
        let pos = mon.position();
        let scale = mon.scale_factor();
        let w = size.width as f64 / scale;
        let h = 92.0;
        let x = pos.x as f64 / scale;
        let y = pos.y as f64 / scale;
        return (w, h, x, y);
    }
    (1280.0, 92.0, 0.0, 0.0)
}

/// 主显示器全屏几何（逻辑坐标）：光圈覆盖层铺满整个主屏。
/// 注意：接管期间主窗口已被最小化，`main.current_monitor()` 在最小化态可能返回 None，
/// 此处改用 app.primary_monitor()（与主窗口状态无关）。
fn full_screen_geometry(app: &AppHandle) -> (f64, f64, f64, f64) {
    if let Ok(Some(mon)) = app.primary_monitor() {
        let size = mon.size();
        let pos = mon.position();
        let scale = mon.scale_factor();
        return (
            size.width as f64 / scale,
            size.height as f64 / scale,
            pos.x as f64 / scale,
            pos.y as f64 / scale,
        );
    }
    (1280.0, 800.0, 0.0, 0.0)
}

/// 包含物理坐标点 (x, y) 的显示器全屏几何（逻辑坐标）；
/// 找不到时回退主显示器。光圈覆盖层需要铺在「目标应用所在」的显示器上。
fn monitor_geometry_at(app: &AppHandle, x: i32, y: i32) -> (f64, f64, f64, f64) {
    if let Ok(monitors) = app.available_monitors() {
        for mon in monitors.iter() {
            let mx = mon.position().x;
            let my = mon.position().y;
            let mw = mon.size().width as i32;
            let mh = mon.size().height as i32;
            if x >= mx && x < mx + mw && y >= my && y < my + mh {
                let scale = mon.scale_factor();
                return (
                    mw as f64 / scale,
                    mh as f64 / scale,
                    mx as f64 / scale,
                    my as f64 / scale,
                );
            }
        }
    }
    full_screen_geometry(app)
}

/// 最小化白泽主窗口（给桌面腾出空间）
pub fn minimize_main_window(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(main) = handle.get_webview_window("main") {
            let _ = main.minimize();
        }
    });
}

/// 恢复并聚焦白泽主窗口
pub fn restore_main_window(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(main) = handle.get_webview_window("main") {
            let _ = main.show();
            let _ = main.unminimize();
            let _ = main.set_focus();
        }
    });
}

/// 确保顶部弹幕横幅浮窗存在并显示（透明、无边框、置顶、鼠标穿透）
pub fn ensure_step_window(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = handle.get_webview_window("step") {
            let _ = win.show();
            let _ = win.set_ignore_cursor_events(true);
            // 重申最高层：防止被其他置顶窗口盖住
            let _ = win.set_always_on_top(true);
            return;
        }
        let (w, h, x, y) = top_bar_geometry(&handle);
        diag_log(&format!(
            "[弹幕] 创建步骤横幅 geometry=({x:.0},{y:.0} {w:.0}x{h:.0})"
        ));
        // 与光圈覆盖层同款创建序列：先隐藏创建 → 配置穿透/置顶 → 再显示。
        // 透明窗口「先显示后设穿透」在部分 WebView2 环境会触发合成层竞态导致进程崩溃
        // （GUI 自动化时上方横幅一出现就崩的主嫌疑）；halo 用此序列已稳定运行数天
        match WebviewWindowBuilder::new(&handle, "step", WebviewUrl::App("index.html#/step".into()))
            .title("白泽 · 步骤")
            .decorations(false)
            .transparent(true)
            .skip_taskbar(true)
            .always_on_top(true)
            .shadow(false)
            .resizable(false)
            .inner_size(w, h)
            .position(x, y)
            .visible(false)
            .build()
        {
            Ok(win) => {
                // 鼠标穿透：弹幕浮窗不拦截对桌面/目标窗口的点击；并锁死最高层
                let _ = win.set_ignore_cursor_events(true);
                let _ = win.set_always_on_top(true);
                let _ = win.show();
                diag_log("[弹幕] 步骤横幅创建成功并已显示");
            }
            Err(e) => {
                eprintln!("[窗口] 创建步骤浮窗失败: {e}");
                diag_log(&format!("[弹幕] 创建步骤横幅失败: {e}"));
            }
        }
    });
}

/// 隐藏弹幕浮窗
pub fn hide_step_window(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = handle.get_webview_window("step") {
            let _ = win.hide();
        }
    });
}

// ───────────────── 目标应用光圈覆盖层（halo） ─────────────────

/// 全屏透明穿透覆盖窗：渲染「目标应用边缘光圈」与「组件点击光环」。
/// 事件协议（halo-event）：{type:"window", rect:[x,y,w,h], title} 圈住目标应用；
/// {type:"flash", x, y, w?, h?} 在被点组件位置闪一圈光环；{type:"clear"} 清除。
pub fn ensure_halo_window(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = handle.get_webview_window("halo") {
            let _ = win.show();
            let _ = win.set_ignore_cursor_events(true);
            let _ = win.set_always_on_top(true);
            return;
        }
        let (w, h, x, y) = full_screen_geometry(&handle);
        eprintln!("[光圈] 创建覆盖层 geometry=({x:.0},{y:.0} {w:.0}x{h:.0})");
        diag_log(&format!("[光圈] 创建覆盖层 geometry=({x:.0},{y:.0} {w:.0}x{h:.0})"));
        match WebviewWindowBuilder::new(&handle, "halo", WebviewUrl::App("index.html#/halo".into()))
            .title("白泽 · 目标光圈")
            .decorations(false)
            .transparent(true)
            .skip_taskbar(true)
            .always_on_top(true)
            .shadow(false)
            .resizable(false)
            .inner_size(w, h)
            .position(x, y)
            .visible(false)
            .build()
        {
            Ok(win) => {
                let _ = win.set_ignore_cursor_events(true);
                let _ = win.set_always_on_top(true);
                let _ = win.show();
                diag_log("[光圈] 覆盖层创建成功并已显示");
            }
            Err(e) => {
                eprintln!("[窗口] 创建光圈覆盖层失败: {e}");
                diag_log(&format!("[光圈] 创建覆盖层失败: {e}"));
            }
        }
    });
}

/// 把光圈覆盖层挪到包含物理坐标点 (x, y) 的显示器上并铺满该屏。
/// 覆盖窗创建时按主屏铺满；目标应用在副屏时必须迁移，否则光圈画在错误的屏上。
fn position_halo_on(app: &AppHandle, x: i32, y: i32) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = handle.get_webview_window("halo") {
            // 已在目标屏上则不折腾（避免每次点击都闪动）
            if let Ok(Some(mon)) = win.current_monitor() {
                let mx = mon.position().x;
                let my = mon.position().y;
                let mw = mon.size().width as i32;
                let mh = mon.size().height as i32;
                if x >= mx && x < mx + mw && y >= my && y < my + mh {
                    return;
                }
            }
            let (w, h, wx, wy) = monitor_geometry_at(&handle, x, y);
            eprintln!("[光圈] 覆盖层迁移至 ({wx:.0},{wy:.0} {w:.0}x{h:.0})");
            diag_log(&format!("[光圈] 覆盖层迁移至 ({wx:.0},{wy:.0} {w:.0}x{h:.0})"));
            let _ = win.set_size(LogicalSize::new(w, h));
            let _ = win.set_position(LogicalPosition::new(wx, wy));
            let _ = win.set_always_on_top(true);
        }
    });
}

/// 最近一次「目标窗口」光圈事件（供覆盖层页面加载后补拉，消除首事件竞态：
/// 覆盖窗创建后页面 JS 订阅完成前 emit 的事件会丢失，导致整轮任务看不到呼吸圈）
static LAST_HALO: OnceLock<Mutex<Option<Value>>> = OnceLock::new();

/// 光圈环绕目标应用窗口（物理屏幕坐标）
pub fn halo_target_window(app: &AppHandle, rect: [i32; 4], title: &str) {
    let event = json!({
        "type": "window",
        "rect": rect,
        "title": title,
    });
    println!("[光圈] 点亮目标「{title}」rect={rect:?}");
    diag_log(&format!("[光圈] 点亮目标「{title}」rect={rect:?}"));
    // 记录最近事件 + 先行迁移覆盖层（run_on_main_thread 按调用顺序执行，先迁移后 emit）
    let cell = LAST_HALO.get_or_init(|| Mutex::new(None));
    if let Ok(mut g) = cell.lock() {
        *g = Some(event.clone());
    }
    ensure_halo_window(app);
    position_halo_on(app, (rect[0] + rect[2] / 2).max(0), (rect[1] + rect[3] / 2).max(0));
    if let Err(e) = app.emit("halo-event", event) {
        eprintln!("[光圈] 事件发送失败: {e}");
    }
}

/// 组件点击光环：在物理屏幕坐标 (x, y) 处闪一圈光环
pub fn halo_flash_at(app: &AppHandle, x: i32, y: i32) {
    ensure_halo_window(app);
    let _ = app.emit("halo-event", json!({ "type": "flash", "x": x, "y": y }));
}

/// 供覆盖层页面挂载时补拉最近的目标光圈事件（可能为 null）
#[tauri::command]
pub fn halo_get_last() -> Value {
    LAST_HALO
        .get()
        .and_then(|cell| cell.lock().ok().and_then(|g| g.clone()))
        .unwrap_or(Value::Null)
}

/// 前端日志透传：覆盖层窗口（halo/step）的 WebView 控制台不可见，
/// 关键诊断信息经此打到后端控制台
#[tauri::command]
pub fn frontend_log(msg: String) {
    println!("[前端] {}", msg.trim());
    diag_log(&format!("[前端] {}", msg.trim()));
}

/// 覆盖层诊断日志（exe 同目录 baize-overlay.log）：双击启动时控制台不可见，
/// 光圈/弹幕链路的关键事件落盘，供事后排查（每次追加写入，频率极低）
pub fn diag_log(line: &str) {
    use std::io::Write;
    let Some(path) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("baize-overlay.log")))
    else {
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{ts} {line}");
    }
}

/// 清除光圈
pub fn halo_clear(app: &AppHandle) {
    if let Some(handle) = app.get_webview_window("halo") {
        let _ = handle.hide();
    }
}

/// 展示一条步骤弹幕工具（自动最小化主窗口 + 弹幕浮窗滚动）
pub struct ShowStepTool {
    app: AppHandle,
}

impl ShowStepTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for ShowStepTool {
    fn name(&self) -> &str {
        "show_step"
    }
    fn description(&self) -> &str {
        "在桌面底部以「直播间弹幕」风格滚动展示当前执行步骤。调用时会自动最小化白泽主窗口并在桌面显示透明浮窗；每完成一步再次调用即推出一条新弹幕。适合 GUI 自动化等耗时/多步任务时让用户直观看到进度（只读，无需授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "要展示的步骤文本，如「正在打开资源管理器…」" }
            },
            "required": ["text"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let text = args["text"].as_str().ok_or("缺少参数 text")?.to_string();
        minimize_main_window(&self.app);
        ensure_step_window(&self.app);
        push_step(&self.app, &text);
        Ok(json!({ "ok": true }))
    }
}

/// 收起弹幕浮窗工具（结束 GUI 自动化后调用）
pub struct HideStepTool {
    app: AppHandle,
}

impl HideStepTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for HideStepTool {
    fn name(&self) -> &str {
        "hide_step"
    }
    fn description(&self) -> &str {
        "收起桌面步骤弹幕浮窗并恢复白泽主窗口，用于 GUI 自动化任务结束后的收尾（只读，无需授权）"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        clear_step_log();
        hide_step_window(&self.app);
        restore_main_window(&self.app);
        Ok(json!({ "ok": true }))
    }
}

/// 读取当前步骤弹幕历史（供浮窗页面加载时补齐）
#[tauri::command]
pub fn get_step_log() -> Vec<String> {
    step_log().lock().unwrap().clone()
}
