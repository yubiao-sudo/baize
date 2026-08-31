//! 屏幕接管：GUI 任务执行期间阻断用户物理键鼠输入，防止外界干扰导致鼠标定位偏移，
//! 同时在桌面底部以「直播间弹幕」浮窗展示进度；按紧急快捷键 Ctrl+Shift+F12 可随时手动解除。
//!
//! 实现要点：
//! - 进程启动后常驻一个低层键鼠钩子线程（WH_KEYBOARD_LL / WH_MOUSE_LL）。
//! - 钩子回调仅在「接管状态」下吞掉 *物理* 键盘/鼠标事件；注入事件（白泽自己的 mouse/键盘操作）
//!   会被放行，因此白泽的自动化操作不受影响。
//! - 物理键盘事件里持续检测 Ctrl+Shift+F12 组合，命中即解除接管并恢复主窗口。

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::OnceLock;

use serde_json::{json, Value};
use tauri::AppHandle;

use crate::tools::{PermissionClass, Tool};
use crate::windows::{
    clear_step_log, ensure_step_window, hide_step_window, minimize_main_window, push_step,
    restore_main_window,
};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, EnumWindows, GetForegroundWindow, GetMessageW, GetWindow,
    GetWindowTextW, GetWindowThreadProcessId, HHOOK, IsWindowVisible, KBDLLHOOKSTRUCT,
    MSLLHOOKSTRUCT, MSG, SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx,
    GW_OWNER, SW_MINIMIZE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
};

/// 是否处于接管状态
static TAKEOVER: AtomicBool = AtomicBool::new(false);
/// 低层键盘钩子句柄
static HOOK_KB: AtomicIsize = AtomicIsize::new(0);
/// 低层鼠标钩子句柄
static HOOK_MS: AtomicIsize = AtomicIsize::new(0);
/// 全局 AppHandle（供钩子线程在紧急解除时恢复窗口）
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
/// 钩子线程是否已启动（幂等）
static HOOK_STARTED: AtomicBool = AtomicBool::new(false);
/// 物理 Ctrl 是否处于按下状态（左/右任意一个）
static CTRL_DOWN: AtomicBool = AtomicBool::new(false);
/// 物理 Shift 是否处于按下状态（左/右任意一个）
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);
/// 最近一次 Agent 活动心跳（毫秒时间戳）：任务看门狗用——
/// 接管激活但长时间无活动（Agent 卡死/退出未释放）时自动解除，防止键鼠永久被锁
static LAST_ACTIVITY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Agent 侧活动心跳：每轮对话、每个工具结果后调用
pub fn touch_activity() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    LAST_ACTIVITY.store(now, Ordering::SeqCst);
}

// 虚拟键码：左右 Ctrl / Shift 以及 F12（vkCode 为 DWORD，与 u32 对应）
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_F12: u32 = 0x7B;
// 注入标志位：键盘 0x10（LLKHF_INJECTED）、鼠标 0x01（LLMHF_INJECTED）
const KBD_INJECTED: u32 = 0x10;
const MS_INJECTED: u32 = 0x01;

/// 初始化：存储 AppHandle 并启动常驻钩子线程（幂等，可在 setup 中多次调用）。
pub fn init(app: AppHandle) {
    let _ = APP_HANDLE.set(app);
    if HOOK_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(hook_thread_main);
    // 任务看门狗：接管激活但 3 分钟无 Agent 活动（Agent 卡死/异常退出未释放）时
    // 自动解除接管并恢复键鼠，防止桌面被永久锁死
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
        if !TAKEOVER.load(Ordering::SeqCst) {
            continue;
        }
        let last = LAST_ACTIVITY.load(Ordering::SeqCst);
        if last == 0 {
            continue;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if now.saturating_sub(last) > 180_000 {
            if let Some(app) = APP_HANDLE.get() {
                let handle = app.clone();
                let _ = handle.clone().run_on_main_thread(move || {
                    crate::windows::push_step(
                        &handle,
                        "⚠ 任务看门狗：3 分钟无活动，已自动解除接管并恢复键鼠",
                    );
                    disengage_screen(&handle);
                });
            }
        }
    });
}

/// 进入接管：阻断外界输入 + 最小化主窗口 + 清屏（最小化无关窗口）+ 显示弹幕浮窗。
/// task_hint：本轮任务文本——用于按语义保留与任务相关的目标窗口（标题词元匹配任务文本）。
pub fn engage_screen(app: &AppHandle, task_hint: Option<&str>) {
    TAKEOVER.store(true, Ordering::SeqCst);
    let _ = APP_HANDLE.set(app.clone());
    touch_activity();
    minimize_main_window(app);
    minimize_irrelevant_windows(app, task_hint);
    // 二次清扫（稳定性）：部分应用会在首次清屏后短暂延迟内弹出/恢复窗口，
    // 600ms 后再扫一遍；期间接管已解除则跳过
    let app2 = app.clone();
    let hint2 = task_hint.map(|s| s.to_string());
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        if TAKEOVER.load(Ordering::SeqCst) {
            minimize_irrelevant_windows(&app2, hint2.as_deref());
        }
    });
    ensure_step_window(app);
    // 推一条接管提示（浮窗内的静态横幅始终显示；此处同时滚动一条弹幕提醒用户）
    push_step(app, "白泽正在执行 GUI 自动化任务，请勿操作（Ctrl+Shift+F12 可紧急解除）");
}

/// 退出接管：恢复外界输入 + 恢复主窗口 + 收起弹幕浮窗 + 清空步骤历史。
/// 接管是否激活（供执行流判断是否把工具步骤推送到桌面弹幕）
pub fn is_active() -> bool {
    TAKEOVER.load(Ordering::SeqCst)
}

pub fn disengage_screen(app: &AppHandle) {
    TAKEOVER.store(false, Ordering::SeqCst);
    CTRL_DOWN.store(false, Ordering::SeqCst);
    SHIFT_DOWN.store(false, Ordering::SeqCst);
    restore_main_window(app);
    hide_step_window(app);
    clear_step_log();
    crate::windows::halo_clear(app);
}

/// 清屏：最小化所有「无关」顶层窗口。保留三类窗口——当前前台窗口（很可能是任务目标）、
/// 本进程窗口，以及「标题与任务文本语义相关」的窗口（按词元匹配，见 title_matches_task）。
fn minimize_irrelevant_windows(app: &AppHandle, task_hint: Option<&str>) {
    let hint = task_hint.unwrap_or("").to_lowercase();
    let app2 = app.clone();
    let _ = app2.clone().run_on_main_thread(move || unsafe {
        let keep = GetForegroundWindow();
        let self_pid = GetCurrentProcessId();
        let mut ctx = CleanCtx {
            self_pid,
            keep,
            hint,
            minimized: 0,
        };
        EnumWindows(
            Some(clean_enum_proc),
            LPARAM(&mut ctx as *mut CleanCtx as isize),
        );
        // 清屏结果汇报到桌面弹幕（执行流可见，二次清扫为 0 时不再重复播报）
        if ctx.minimized > 0 {
            crate::windows::push_step(
                &app2,
                &format!("清屏：已最小化 {} 个无关窗口", ctx.minimized),
            );
        }
    });
}

/// 清屏枚举上下文
struct CleanCtx {
    self_pid: u32,
    keep: HWND,
    /// 本轮任务文本（小写）：标题语义匹配用
    hint: String,
    /// 本次清扫实际最小化的窗口数（验证/汇报用）
    minimized: usize,
}

/// 从窗口标题提取「词元」：CJK 连续段（≥2 字，保留原样）与拉丁字母数字词（≥3 字符，转小写）。
/// 例：「记事本 - Notepad」→ ["记事本", "notepad"]；「Docker Desktop」→ ["docker", "desktop"]。
fn title_tokens(title: &str) -> Vec<String> {
    fn is_cjk(ch: char) -> bool {
        let c = ch as u32;
        (0x4E00..=0x9FFF).contains(&c) || (0x3400..=0x4DBF).contains(&c)
    }
    fn flush(cur: &mut String, cjk: bool, out: &mut Vec<String>) {
        if !cur.is_empty() && cur.chars().count() >= if cjk { 2 } else { 3 } {
            out.push(cur.to_lowercase());
        }
        cur.clear();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_cjk = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            let cjk = is_cjk(ch);
            if !cur.is_empty() && cjk != cur_cjk {
                flush(&mut cur, cur_cjk, &mut out);
            }
            cur.push(ch);
            cur_cjk = cjk;
        } else {
            flush(&mut cur, cur_cjk, &mut out);
        }
    }
    flush(&mut cur, cur_cjk, &mut out);
    out
}

/// 窗口标题是否与任务文本语义相关：标题的任一词元出现在任务文本中即视为相关。
/// 例：任务「帮我在记事本里输入 hello」会保留标题为「记事本 / Notepad」的窗口。
fn title_matches_task(title: &str, hint: &str) -> bool {
    if hint.is_empty() {
        return false;
    }
    title_tokens(title)
        .iter()
        .any(|token| hint.contains(token.as_str()))
}

unsafe extern "system" fn clean_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut CleanCtx);
    // 跳过不可见窗口
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    // 跳过拥有者窗口（其他窗口的工具窗/弹出窗）
    if GetWindow(hwnd, GW_OWNER).0 != 0 {
        return BOOL(1);
    }
    // 保留前台窗口（很可能是任务目标）
    if hwnd == ctx.keep {
        return BOOL(1);
    }
    // 保留本进程窗口（白泽主窗口、浏览器、文档、终端等）
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == ctx.self_pid {
        return BOOL(1);
    }
    // 保留标题与任务语义相关的窗口：按任务文本匹配窗口标题词元，
    // 目标应用即便不是当时的前台窗口也不会被清屏误伤
    let mut buf = [0u16; 512];
    let len = GetWindowTextW(hwnd, &mut buf);
    if len > 0 && title_matches_task(&String::from_utf16_lossy(&buf[..len as usize]), &ctx.hint) {
        return BOOL(1);
    }
    ShowWindow(hwnd, SW_MINIMIZE);
    ctx.minimized += 1;
    BOOL(1)
}

/// 钩子线程主循环：安装低层钩子 → 泵消息 → 卸载钩子。
fn hook_thread_main() {
    unsafe {
        let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), HMODULE(0), 0)
            .unwrap_or(HHOOK(0));
        let ms = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), HMODULE(0), 0)
            .unwrap_or(HHOOK(0));
        HOOK_KB.store(kb.0, Ordering::SeqCst);
        HOOK_MS.store(ms.0, Ordering::SeqCst);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND(0), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }

        if HOOK_KB.load(Ordering::SeqCst) != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(HOOK_KB.load(Ordering::SeqCst)));
        }
        if HOOK_MS.load(Ordering::SeqCst) != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(HOOK_MS.load(Ordering::SeqCst)));
        }
    }
}

/// 紧急解除：清除接管标志并在主线程恢复窗口。
fn release_now() {
    TAKEOVER.store(false, Ordering::SeqCst);
    CTRL_DOWN.store(false, Ordering::SeqCst);
    SHIFT_DOWN.store(false, Ordering::SeqCst);
    if let Some(app) = APP_HANDLE.get() {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            restore_main_window(&handle);
            hide_step_window(&handle);
        });
    }
}

/// 低层键盘钩子回调。
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let kbd = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let injected = (kbd.flags.0 & KBD_INJECTED) != 0;
        let msg = wparam.0 as u32;
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        let vk = kbd.vkCode;

        // 无论是否接管，都准确跟踪 Ctrl / Shift 的物理按下状态（含左右键），
        // 避免依赖 GetAsyncKeyState 的异步态导致组合键误判。
        match vk {
            VK_LCONTROL | VK_RCONTROL => {
                if is_down {
                    CTRL_DOWN.store(true, Ordering::SeqCst);
                } else if is_up {
                    CTRL_DOWN.store(false, Ordering::SeqCst);
                }
            }
            VK_LSHIFT | VK_RSHIFT => {
                if is_down {
                    SHIFT_DOWN.store(true, Ordering::SeqCst);
                } else if is_up {
                    SHIFT_DOWN.store(false, Ordering::SeqCst);
                }
            }
            _ => {}
        }

        if TAKEOVER.load(Ordering::SeqCst) && !injected {
            // 紧急解除：Ctrl + Shift + F12 同时按下即退出接管、恢复窗口。
            // 除本钩子维护的物理键状态外，再用 GetAsyncKeyState 兜底校验——
            // 钩子偶尔漏记某次 keydown（如接管瞬间组合键已按住）时仍能可靠解除
            if is_down && vk == VK_F12 {
                use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
                let async_down = |vk: u32| {
                    // 低层钩子回调中调用 GetAsyncKeyState 是安全的（同步查询异步键态）
                    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
                };
                let ctrl = CTRL_DOWN.load(Ordering::SeqCst)
                    || async_down(VK_LCONTROL)
                    || async_down(VK_RCONTROL);
                let shift = SHIFT_DOWN.load(Ordering::SeqCst)
                    || async_down(VK_LSHIFT)
                    || async_down(VK_RSHIFT);
                if ctrl && shift {
                    release_now();
                    return LRESULT(1); // 吞掉触发键
                }
            }
            // 接管期间吞掉所有物理键盘输入
            return LRESULT(1);
        }
    }
    CallNextHookEx(HHOOK(HOOK_KB.load(Ordering::SeqCst)), code, wparam, lparam)
}

/// 低层鼠标钩子回调。
unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let injected = (ms.flags & MS_INJECTED) != 0;
        if TAKEOVER.load(Ordering::SeqCst) && !injected {
            // 接管期间吞掉所有物理鼠标输入
            return LRESULT(1);
        }
    }
    CallNextHookEx(HHOOK(HOOK_MS.load(Ordering::SeqCst)), code, wparam, lparam)
}

// ───────────────────────── 工具 ─────────────────────────

/// 屏幕接管工具（通常由系统在 GUI 任务时自动调用，也供模型在需要时手动调用）
pub struct ScreenTakeoverTool {
    app: AppHandle,
}

impl ScreenTakeoverTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for ScreenTakeoverTool {
    fn name(&self) -> &str {
        "screen_takeover"
    }
    fn description(&self) -> &str {
        "接管整个屏幕以执行 GUI 任务：阻断外界键盘鼠标输入、按任务语义清屏（保留目标应用窗口）、最小化白泽主窗口并在桌面底部显示步骤弹幕，避免操作中被误触或窗口遮挡导致定位偏移。按 Ctrl+Shift+F12 可随时手动解除。凡涉及操作桌面应用/窗口的任务（打开关闭程序、在应用界面里点击输入、切换窗口等），若屏幕尚未接管，应先调用此工具再执行 GUI 操作（只读，无需授权）"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        engage_screen(&self.app, None);
        Ok(json!({ "ok": true, "message": "已接管屏幕，按 Ctrl+Shift+F12 解除" }))
    }
}

/// 屏幕解除工具（结束 GUI 任务后收回控制权）
pub struct ScreenReleaseTool {
    app: AppHandle,
}

impl ScreenReleaseTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for ScreenReleaseTool {
    fn name(&self) -> &str {
        "screen_release"
    }
    fn description(&self) -> &str {
        "解除屏幕接管：恢复外界键盘鼠标输入、恢复白泽主窗口并收起步骤弹幕。GUI 任务结束或需要把控制权交还用户时调用（只读，无需授权）"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        disengage_screen(&self.app);
        Ok(json!({ "ok": true, "message": "已解除接管" }))
    }
}