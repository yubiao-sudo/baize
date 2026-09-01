//! Windows UIA（UI Automation）无障碍树实现

use super::*;
use tauri::AppHandle;
use arboard::Clipboard;
use uiautomation::controls::ControlType;
use uiautomation::core::UICondition;
use uiautomation::inputs::Keyboard;
use uiautomation::types::TreeScope;
use uiautomation::{UIElement, UIAutomation};
use ::windows::Win32::UI::Input::KeyboardAndMouse::{
    self, keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindow, GetWindowLongW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SetForegroundWindow, SetWindowPos, ShowWindow, GWL_EXSTYLE, GW_OWNER,
    HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOW,
    WS_EX_TOPMOST,
};
use ::windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use ::windows::Win32::System::Threading::GetCurrentProcessId;
// keydown 标志位值为 0，windows crate 未导出该常量，这里自行定义
const KEYEVENTF_KEYDOWN: KEYBD_EVENT_FLAGS = KEYBD_EVENT_FLAGS(0);

pub struct WindowsCapability {
    /// setup 阶段经 capability::init_capability_app 注入（光圈事件需要）
    app: Option<AppHandle>,
}

impl WindowsCapability {
    pub fn new(app: Option<AppHandle>) -> Self {
        Self { app }
    }

    fn connect(&self) -> Result<(UIAutomation, UICondition), CapError> {
        let automation = UIAutomation::new().map_err(|e| CapError::InvalidState(e.to_string()))?;
        let condition = automation
            .create_true_condition()
            .map_err(|e| CapError::InvalidState(e.to_string()))?;
        Ok((automation, condition))
    }

    fn to_rect(r: uiautomation::types::Rect) -> Rect {
        Rect {
            x: r.get_left() as f64,
            y: r.get_top() as f64,
            width: (r.get_right() - r.get_left()).max(0) as f64,
            height: (r.get_bottom() - r.get_top()).max(0) as f64,
        }
    }

    /// 解析根元素：None=前台窗口，Some=指定窗口名
    fn resolve_root(
        &self,
        automation: &UIAutomation,
        condition: &UICondition,
        req: &ObserveReq,
    ) -> Result<UIElement, CapError> {
        match &req.window {
            None => {
                let focused = automation
                    .get_focused_element()
                    .map_err(|e| CapError::InvalidState(format!("无法获取焦点元素: {e}")))?;
                let mut root = focused;
                if let Ok(ancestors) = root.find_all(TreeScope::Ancestors, condition) {
                    if let Some(win) = ancestors
                        .into_iter()
                        .find(|a| matches!(a.get_control_type(), Ok(ControlType::Window)))
                    {
                        root = win;
                    }
                }
                Ok(root)
            }
            Some(WindowTarget::ByName(name)) => {
                let desktop = automation
                    .get_root_element()
                    .map_err(|e| CapError::InvalidState(e.to_string()))?;
                let children = desktop
                    .find_all(TreeScope::Children, condition)
                    .map_err(|e| CapError::InvalidState(e.to_string()))?;
                children
                    .into_iter()
                    .find(|c| c.get_name().unwrap_or_default().contains(name))
                    .ok_or_else(|| CapError::NotFound(format!("未找到窗口: {name}")))
            }
        }
    }
}

impl Capability for WindowsCapability {
    fn probe(&self) -> CapabilitySet {
        CapabilitySet {
            a11y: true,
            screenshot: false, // 截屏后续接入
            input: false,      // M4 接入
        }
    }

    fn list_windows(&self) -> Result<Vec<WindowInfo>, CapError> {
        // 用 Win32 EnumWindows 枚举所有顶层窗口（含拥有者弹出的对话框、空标题弹窗）。
        // 之前只用 UIA desktop 直系子节点且跳过空标题，导致卸载/安装时弹出的确认对话框
        // （#32770，往往有 owner、标题可能为空）被漏掉，白泽「看不到」弹窗。
        let mut ctx = WindowEnumCtx { wins: Vec::new() };
        unsafe {
            EnumWindows(
                Some(window_enum_proc),
                LPARAM(&mut ctx as *mut WindowEnumCtx as isize),
            );
        }
        Ok(ctx.wins)
    }

    fn observe(&self, req: &ObserveReq) -> Result<Observation, CapError> {
        let (automation, condition) = self.connect()?;
        let root = self.resolve_root(&automation, &condition, req)?;

        let mut count = 0usize;
        let mut truncated = false;
        let root_node = build(&root, 0, req, &condition, &mut count, &mut truncated)?;

        Ok(Observation {
            source: "windows_uia".to_string(),
            tree: Some(A11yTree {
                root: root_node,
                node_count: count,
                truncated,
            }),
        })
    }

    fn capture_screen(&self) -> Result<ScreenshotInfo, CapError> {
        let monitors = xcap::Monitor::all()
            .map_err(|e| CapError::InvalidState(format!("枚举显示器失败: {e}")))?;
        if monitors.is_empty() {
            return Err(CapError::InvalidState("未找到显示器".to_string()));
        }

        // 定位鼠标光标当前所在的显示器（多显示器标定：记录该屏物理偏移）
        let mut pt = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut pt) };
        let (cx, cy) = (pt.x, pt.y);
        let monitor = monitors
            .iter()
            .find(|m| {
                let x = m.x().unwrap_or(0);
                let y = m.y().unwrap_or(0);
                let w = m.width().unwrap_or(0) as i32;
                let h = m.height().unwrap_or(0) as i32;
                cx >= x && cx < x + w && cy >= y && cy < y + h
            })
            .unwrap_or(&monitors[0]);
        let offset_x = monitor.x().unwrap_or(0);
        let offset_y = monitor.y().unwrap_or(0);

        let img = monitor
            .capture_image()
            .map_err(|e| CapError::InvalidState(format!("截屏失败: {e}")))?;
        let (w, h) = (img.width(), img.height());

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let name = format!("baize-screenshot-{ts}.png");
        img.save(&name)
            .map_err(|e| CapError::InvalidState(format!("保存截图失败: {e}")))?;
        let path = std::env::current_dir()
            .map(|d| d.join(&name).to_string_lossy().to_string())
            .unwrap_or(name);

        Ok(ScreenshotInfo {
            path,
            width: w,
            height: h,
            offset_x,
            offset_y,
        })
    }

    fn act(&self, action: &Action) -> Result<ActionResult, CapError> {
        match action {
            Action::ReadOnly => Ok(ActionResult {
                ok: true,
                description: "只读".to_string(),
            }),
            Action::MiddleClick { x, y } => {
                middle_click(*x as i32, *y as i32);
                Ok(ActionResult {
                    ok: true,
                    description: format!("中键点击坐标 ({x}, {y})"),
                })
            }
            Action::Hover { x, y } => {
                mouse_move_abs(*x as i32, *y as i32);
                Ok(ActionResult {
                    ok: true,
                    description: format!("悬停坐标 ({x}, {y})"),
                })
            }
            Action::WheelScroll { clicks, horizontal } => {
                wheel_scroll(*clicks, *horizontal);
                let dir = if *horizontal {
                    if *clicks > 0 { "右" } else { "左" }
                } else if *clicks > 0 {
                    "上"
                } else {
                    "下"
                };
                Ok(ActionResult {
                    ok: true,
                    description: format!("滚轮{}滚动 {} 格", dir, clicks.abs()),
                })
            }
            Action::ClickAt { x, y } => {
                click_left(*x as i32, *y as i32);
                Ok(ActionResult {
                    ok: true,
                    description: format!("点击坐标 ({x}, {y})"),
                })
            }
            Action::DoubleClick { x, y } => {
                double_click_left(*x as i32, *y as i32);
                Ok(ActionResult {
                    ok: true,
                    description: format!("双击坐标 ({x}, {y})"),
                })
            }
            Action::RightClick { x, y } => {
                click_right(*x as i32, *y as i32);
                Ok(ActionResult {
                    ok: true,
                    description: format!("右键坐标 ({x}, {y})"),
                })
            }
            Action::Drag {
                from_x,
                from_y,
                to_x,
                to_y,
            } => {
                drag_mouse(
                    *from_x as i32,
                    *from_y as i32,
                    *to_x as i32,
                    *to_y as i32,
                )
                .map_err(|e| CapError::InvalidState(format!("拖拽失败: {e}")))?;
                Ok(ActionResult {
                    ok: true,
                    description: format!("拖拽 ({from_x},{from_y}) → ({to_x},{to_y})"),
                })
            }
            Action::TypeText { text } => {
                Keyboard::new()
                    .send_keys(text)
                    .map_err(|e| CapError::InvalidState(format!("输入失败: {e}")))?;
                Ok(ActionResult {
                    ok: true,
                    description: format!("输入文本: {text}"),
                })
            }
            Action::KeyPress { keys } => {
                let normalized = normalize_keys(keys);
                Keyboard::new()
                    .send_keys(&normalized)
                    .map_err(|e| CapError::InvalidState(format!("按键失败: {e}")))?;
                Ok(ActionResult {
                    ok: true,
                    description: format!("按键: {keys}"),
                })
            }
            Action::KeyDown { key } => {
                let vk = str_to_vk(key)
                    .ok_or_else(|| CapError::InvalidState(format!("未知键名: {key}")))?;
                unsafe {
                    keybd_event(vk.0 as u8, 0, KEYEVENTF_KEYDOWN, 0);
                }
                Ok(ActionResult {
                    ok: true,
                    description: format!("按住键: {key}"),
                })
            }
            Action::KeyUp { key } => {
                let vk = str_to_vk(key)
                    .ok_or_else(|| CapError::InvalidState(format!("未知键名: {key}")))?;
                unsafe {
                    keybd_event(vk.0 as u8, 0, KEYEVENTF_KEYUP, 0);
                }
                Ok(ActionResult {
                    ok: true,
                    description: format!("抬起键: {key}"),
                })
            }
            Action::PasteText { text } => {
                paste_via_clipboard(text)
                    .map_err(|e| CapError::InvalidState(format!("粘贴失败: {e}")))?;
                Ok(ActionResult {
                    ok: true,
                    description: format!("粘贴文本（{} 字符）", text.chars().count()),
                })
            }
            Action::SaveDialog { path } => {
                let (ok, note) = operate_save_dialog(path)
                    .map_err(|e| CapError::InvalidState(format!("另存为对话框操作失败: {e}")))?;
                Ok(ActionResult {
                    ok,
                    description: note,
                })
            }
            Action::WindowMinimizeAll { except } => {
                let mut ctx = MinimizeCtx {
                    except: except.clone(),
                    minimized: Vec::new(),
                };
                unsafe {
                    EnumWindows(
                        Some(minimize_enum_proc),
                        LPARAM(&mut ctx as *mut MinimizeCtx as isize),
                    );
                }
                Ok(ActionResult {
                    ok: true,
                    description: if ctx.minimized.is_empty() {
                        "已最小化所有窗口".to_string()
                    } else {
                        format!("已最小化 {} 个窗口", ctx.minimized.len())
                    },
                })
            }
            Action::WindowSetTopmost { name, topmost } => {
                if set_topmost_by_name(name, *topmost) {
                    // 置顶目标 = 锁定为操作对象：点亮边缘光圈（取消置顶则熄灭）
                    if *topmost {
                        if let (Some(app), Some(rect)) = (&self.app, find_window_rect(name)) {
                            crate::windows::halo_target_window(app, rect, name);
                        }
                    } else {
                        if let Some(app) = &self.app {
                            crate::windows::halo_clear(app);
                        }
                    }
                    Ok(ActionResult {
                        ok: true,
                        description: format!(
                            "已把「{name}」{}",
                            if *topmost { "置顶" } else { "取消置顶" }
                        ),
                    })
                } else {
                    Err(CapError::NotFound(format!("未找到窗口: {name}")))
                }
            }
            Action::WindowFocus { name } => {
                if focus_window_by_name(name) {
                    // 目标应用边缘点亮光圈，提示「这是正在被操作的应用」
                    match find_window_rect(name) {
                        Some(rect) => {
                            if let Some(app) = &self.app {
                                crate::windows::halo_target_window(app, rect, name);
                            }
                        }
                        None => {
                            crate::windows::diag_log(&format!(
                                "[光圈] 点亮跳过(window_focus)：聚焦成功但 find_window_rect 未找到「{name}」"
                            ));
                        }
                    }
                    Ok(ActionResult {
                        ok: true,
                        description: format!("已聚焦窗口: {name}"),
                    })
                } else {
                    Err(CapError::NotFound(format!("未找到窗口: {name}")))
                }
            }
            Action::WindowPrepare { name, topmost } => {
                let Some(hwnd) = find_window_by_name(name) else {
                    return Err(CapError::NotFound(format!("未找到窗口: {name}")));
                };
                // 1) 清屏：最小化除目标与本进程外的所有顶层窗口
                let mut ctx = PrepareCtx {
                    self_pid: unsafe { GetCurrentProcessId() },
                    keep: hwnd,
                    minimized: 0,
                };
                unsafe {
                    EnumWindows(
                        Some(prepare_enum_proc),
                        LPARAM(&mut ctx as *mut PrepareCtx as isize),
                    );
                }
                // 2) 聚焦 + 验证（前台切换失败自动重试）
                let focused = focus_window_verified(hwnd);
                // 3) 置顶 + 验证
                let topmost_ok = set_topmost_hwnd(hwnd, *topmost);
                if focused {
                    // 目标应用边缘点亮光圈，提示「这是正在被操作的应用」
                    match find_window_rect(name) {
                        Some(rect) => {
                            if let Some(app) = &self.app {
                                crate::windows::halo_target_window(app, rect, name);
                            }
                        }
                        None => {
                            crate::windows::diag_log(&format!(
                                "[光圈] 点亮跳过：聚焦成功但 find_window_rect 未找到「{name}」"
                            ));
                        }
                    }
                } else {
                    crate::windows::diag_log(&format!(
                        "[光圈] 聚焦验证失败 name={name} 当前前台=「{}」",
                        foreground_title()
                    ));
                }
                let fg_title = foreground_title();
                let mut desc = format!(
                    "清屏准备：已最小化 {} 个无关窗口；",
                    ctx.minimized
                );
                if focused {
                    desc.push_str(&format!("聚焦「{name}」成功；"));
                } else {
                    desc.push_str(&format!(
                        "聚焦「{name}」失败（当前前台是「{fg_title}」，可能被系统前台锁拦截）；"
                    ));
                }
                if *topmost {
                    desc.push_str(if topmost_ok { "置顶成功" } else { "置顶失败" });
                }
                Ok(ActionResult {
                    ok: focused,
                    description: desc,
                })
            }
        }
    }

    fn find(&self, target: &str) -> Result<Vec<ElementMatch>, CapError> {
        let (automation, condition) = self.connect()?;
        let req = ObserveReq::default();
        let root = self.resolve_root(&automation, &condition, &req)?;
        let mut matches = Vec::new();
        collect_matches(&root, target, &condition, 0, 6, &mut matches);
        Ok(matches)
    }

    fn interactive_map(&self, window: Option<String>) -> Result<Value, CapError> {
        let (automation, condition) = self.connect()?;
        // 根元素：指定窗口名 → 按名解析；否则取当前焦点应用的顶层窗口
        let req = ObserveReq {
            mode: crate::capability::ObserveMode::TreeOnly,
            max_depth: 0,
            max_nodes: 0,
            window: window
                .map(crate::capability::WindowTarget::ByName),
        };
        let root = self.resolve_root(&automation, &condition, &req)?;

        let mut out: Vec<Value> = Vec::new();

        fn walk(
            el: &UIElement,
            condition: &UICondition,
            depth: usize,
            out: &mut Vec<Value>,
        ) -> Result<(), CapError> {
            if depth > 14 || out.len() >= 120 {
                return Ok(());
            }
            let ct = el
                .get_control_type()
                .map(|c| format!("{:?}", c))
                .unwrap_or_default();
            let bbox = el.get_bounding_rectangle().ok();
            // Image 控件单独收录：音乐/视频类自渲染应用的「播放按钮」是行内小图标（无文字、
            // 不可交互类型），但它正是点击播放的目标——只收图标尺寸（8-64px），避免大封面图刷屏
            let icon_image = ct == "Image"
                && bbox
                    .as_ref()
                    .map(|r| {
                        let w = r.get_right() - r.get_left();
                        let h = r.get_bottom() - r.get_top();
                        (8..=64).contains(&w) && (8..=64).contains(&h)
                    })
                    .unwrap_or(false);
            if interactive_ctl(&ct) || icon_image {
                if let Ok(name) = el.get_name() {
                    let name = name.trim().to_string();
                    if !name.is_empty() || icon_image {
                        let (cx, cy) = bbox
                            .map(|r| {
                                (
                                    (r.get_left() + r.get_right()) as f64 / 2.0,
                                    (r.get_top() + r.get_bottom()) as f64 / 2.0,
                                )
                            })
                            .unwrap_or((0.0, 0.0));
                        out.push(json!({
                            "name": if name.is_empty() { format!("{ct}图标") } else { name },
                            "type": ct,
                            "x": cx as i32,
                            "y": cy as i32,
                        }));
                    }
                }
            }
            if let Ok(children) = el.find_all(TreeScope::Children, condition) {
                for c in children {
                    walk(&c, condition, depth + 1, out)?;
                }
            }
            Ok(())
        }
        walk(&root, &condition, 0, &mut out)?;

        Ok(json!({ "elements": out, "count": out.len() }))
    }

    fn find_anywhere(&self, target: &str) -> Result<Vec<ElementMatch>, CapError> {
        // 跨所有顶层窗口搜索（而非仅前台窗口）：遍历 desktop 的每棵子树，
        // 使弹出对话框（确认/卸载/错误）里的按钮也能被定位并点击。
        let (automation, condition) = self.connect()?;
        let desktop = automation
            .get_root_element()
            .map_err(|e| CapError::InvalidState(e.to_string()))?;
        let children = desktop
            .find_all(TreeScope::Children, &condition)
            .map_err(|e| CapError::InvalidState(e.to_string()))?;
        let mut matches = Vec::new();
        for c in children {
            collect_matches(&c, target, &condition, 0, 6, &mut matches);
        }
        matches.sort_by(|a, b| b.score.cmp(&a.score));
        Ok(matches)
    }

    fn click_element(&self, target: &str) -> Result<ActionResult, CapError> {
        let (automation, condition) = self.connect()?;
        let req = ObserveReq::default();
        let root = self.resolve_root(&automation, &condition, &req)?;
        // 评分选优：先收集全部候选取最高分，再回树上按「名称+矩形」双条件定位点击。
        // 旧的「首个 name.contains 即点」会点中同名容器/父级文本，导致点空。
        let mut candidates = Vec::new();
        collect_matches(&root, target, &condition, 0, 6, &mut candidates);
        candidates.sort_by(|a, b| b.score.cmp(&a.score));
        if let Some(best) = candidates.first() {
            let (name, bbox, score) = (best.name.clone(), best.bbox, best.score);
            let mut clicked = false;
            click_by_ident(&root, &condition, 0, 6, &name, bbox, &mut clicked)?;
            if clicked {
                let note = if score < 72 {
                    format!("（模糊匹配 score={score}，请核验是否点中预期控件）")
                } else {
                    String::new()
                };
                return Ok(ActionResult {
                    ok: true,
                    description: format!("点击控件: {target}{note}"),
                });
            }
        }
        // 回退：OCR / Set-of-Marks / 视觉接地 → 坐标点击，并附操作后界面变化验证。
        // 截图复用：ground_on_screenshot 直接用本图，点击后再截一次做 diff（共 2 次截图）。
        let info = self.capture_screen()?;
        let rect = super::ground_on_screenshot(self, target, &info)?;
        let cx = rect.x + rect.width / 2.0;
        let cy = rect.y + rect.height / 2.0;
        let _res = self.act(&Action::ClickAt { x: cx, y: cy })?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        let after = self.capture_screen().ok();
        let mut diff = after
            .as_ref()
            .map(|a| crate::som::image_diff_pct(&info.path, &a.path))
            .unwrap_or(100.0); // 截屏失败视为有变化，不触发重试
        // 微偏移重试：中心点击后界面几乎无变化时，向右下偏 3px 再点一次——
        // 消除「目标中心恰好在控件缝隙/禁用区」造成的贴边未命中
        let mut retried = false;
        if diff < 1.0 {
            let (jx, jy) = (cx + 3.0, cy + 3.0);
            let _ = self.act(&Action::ClickAt { x: jx, y: jy });
            std::thread::sleep(std::time::Duration::from_millis(300));
            if let Ok(after2) = self.capture_screen() {
                let diff2 = crate::som::image_diff_pct(&info.path, &after2.path);
                if diff2 > diff {
                    diff = diff2;
                    retried = true;
                }
            }
        }
        let note = if diff < 1.0 {
            format!("坐标点击已执行（含微偏移重试），但界面几乎无变化（{diff:.1}%），可能未命中目标")
        } else if retried {
            format!("中心点击无变化，微偏移重试后界面变化 {diff:.1}%（已命中）")
        } else {
            format!("坐标点击已执行，界面变化 {diff:.1}%")
        };
        Ok(ActionResult {
            ok: true,
            description: format!("点击控件: {target}（{note}）"),
        })
    }
}

/// 判定控件类型是否为可交互元素（元素地图只收录这些）
fn interactive_ctl(ct: &str) -> bool {
    matches!(
        ct,
        "Button"
            | "Hyperlink"
            | "Edit"
            | "ComboBox"
            | "CheckBox"
            | "RadioButton"
            | "ListItem"
            | "MenuItem"
            | "TabItem"
            | "TreeItem"
            | "Spinner"
            | "Tab"
            | "Menu"
    )
}

fn build(
    el: &UIElement,
    depth: usize,
    req: &ObserveReq,
    condition: &UICondition,
    count: &mut usize,
    truncated: &mut bool,
) -> Result<A11yNode, CapError> {
    if *count >= req.max_nodes {
        *truncated = true;
        return Ok(leaf("…(截断)"));
    }
    *count += 1;

    let role = el
        .get_control_type()
        .map(|c| format!("{:?}", c))
        .unwrap_or_default();
    let name = el.get_name().unwrap_or_default();
    let bbox = el.get_bounding_rectangle().ok().map(WindowsCapability::to_rect);
    let enabled = el.is_enabled().unwrap_or(true);

    let mut node = A11yNode {
        role,
        name,
        value: None,
        bbox,
        enabled,
        focused: false,
        children: Vec::new(),
    };

    if depth < req.max_depth {
        if let Ok(children) = el.find_all(TreeScope::Children, condition) {
            for c in children {
                let child = build(&c, depth + 1, req, condition, count, truncated)?;
                if !child.role.is_empty() || !child.children.is_empty() {
                    node.children.push(child);
                }
            }
        }
    }

    Ok(node)
}

fn collect_matches(
    el: &UIElement,
    target: &str,
    condition: &UICondition,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<ElementMatch>,
) {
    if depth > max_depth {
        return;
    }
    let name = el.get_name().unwrap_or_default();
    let bbox = el.get_bounding_rectangle().ok().map(WindowsCapability::to_rect);
    let role = el
        .get_control_type()
        .map(|c| format!("{c:?}"))
        .unwrap_or_default();
    if !name.is_empty() {
        // 角色参与评分：目标写明「按钮/输入框」等类型词时，对应控件加分
        let score = super::match_score(&name, &role, target);
        if score > 0 {
            out.push(ElementMatch { name, bbox, score });
        }
    } else if role == "Image" {
        // 无名小图标（音乐/视频应用的行首播放按钮等）：以合成名「Image图标」参与匹配，
        // 让 find_element("Image图标")/ground_element("Image图标") 能拿到全部图标精确坐标
        let icon_sized = bbox
            .as_ref()
            .map(|r| (8.0..=64.0).contains(&r.width) && (8.0..=64.0).contains(&r.height))
            .unwrap_or(false);
        if icon_sized {
            let synth = "Image图标";
            let score = super::match_score(synth, &role, target);
            if score > 0 {
                out.push(ElementMatch {
                    name: synth.to_string(),
                    bbox,
                    score,
                });
            }
        }
    }
    if let Ok(children) = el.find_all(TreeScope::Children, condition) {
        for c in children {
            collect_matches(&c, target, condition, depth + 1, max_depth, out);
        }
    }
}

/// 按「名称 + 矩形」双条件回树定位并点击（评分选优后的第二遍遍历）
fn click_by_ident(
    el: &UIElement,
    condition: &UICondition,
    depth: usize,
    max_depth: usize,
    name: &str,
    bbox: Option<Rect>,
    clicked: &mut bool,
) -> Result<(), CapError> {
    if *clicked || depth > max_depth {
        return Ok(());
    }
    if el.get_name().unwrap_or_default() == name {
        let same_pos = match (bbox, el.get_bounding_rectangle().ok().map(WindowsCapability::to_rect)) {
            (Some(a), Some(b)) => {
                (a.x - b.x).abs() < 2.0
                    && (a.y - b.y).abs() < 2.0
                    && (a.width - b.width).abs() < 2.0
                    && (a.height - b.height).abs() < 2.0
            }
            (None, _) | (_, None) => true,
        };
        if same_pos {
            el.click()
                .map_err(|e| CapError::InvalidState(format!("点击控件失败: {e}")))?;
            *clicked = true;
            return Ok(());
        }
    }
    if let Ok(children) = el.find_all(TreeScope::Children, condition) {
        for c in children {
            click_by_ident(&c, condition, depth + 1, max_depth, name, bbox, clicked)?;
            if *clicked {
                break;
            }
        }
    }
    Ok(())
}

fn leaf(role: &str) -> A11yNode {
    A11yNode {
        role: role.to_string(),
        name: String::new(),
        value: None,
        bbox: None,
        enabled: false,
        focused: false,
        children: Vec::new(),
    }
}

// ───────────────────── 鼠标事件注入（SendInput 绝对坐标版） ─────────────────────
//
// 之前用 `SetCursorPos` + `mouse_event(LEFTDOWN)` 分两步注入：mouse_event 已被微软
// 弃用，且「按下」时依赖系统记录的瞬时光标位置，存在定位偏差（尤其高 DPI / 多显示器）。
// 这里改用 SendInput，把「移动 + 按下」合并成一条绝对坐标事件，直接命中目标像素。

use ::windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS;

/// 屏幕物理坐标 → SendInput 需要的 0..=65535 归一化绝对坐标（基于虚拟屏幕边界，
/// 兼容副屏负坐标与多显示器布局）。
/// 精度说明：按 MS 文档 0 映射左边缘、65535 映射右边缘，正确公式是 *65535/sw；
/// 旧式 /(sw-1) 会在全屏范围引入 ≤1px 的系统性偏差，此处改为四舍五入并钳制，
/// 输入坐标也钳制进虚拟屏幕，避免越界点击落到错误的显示器。
fn abs_mouse_pos(x: i32, y: i32) -> (i32, i32) {
    use ::windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        let sx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let sy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let sw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let sh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        let x = x.clamp(sx, sx + sw - 1);
        let y = y.clamp(sy, sy + sh - 1);
        let dx = if sw > 1 {
            (((x - sx) as f64 * 65535.0 / sw as f64).round() as i64).clamp(0, 65535) as i32
        } else {
            0
        };
        let dy = if sh > 1 {
            (((y - sy) as f64 * 65535.0 / sh as f64).round() as i64).clamp(0, 65535) as i32
        } else {
            0
        };
        (dx, dy)
    }
}

/// 注入一条鼠标输入事件（dx/dy 为归一化绝对坐标，flags 决定动作）。
fn send_mouse(dx: i32, dy: i32, flags: MOUSE_EVENT_FLAGS) -> u32 {
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT,
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) }
}

/// 仅移动光标到目标（触发 hover 状态，不点击）。
fn mouse_move_abs(x: i32, y: i32) {
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE};
    let (dx, dy) = abs_mouse_pos(x, y);
    send_mouse(dx, dy, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE);
}

/// 拖拽：移动到起点并按下 → 分步移动到终点 → 释放（全程绝对坐标）。
fn drag_mouse(from_x: i32, from_y: i32, to_x: i32, to_y: i32) -> Result<(), String> {
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE,
    };
    let (sx, sy) = abs_mouse_pos(from_x, from_y);
    send_mouse(
        sx,
        sy,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTDOWN,
    );
    std::thread::sleep(std::time::Duration::from_millis(40));
    let steps = 24;
    for i in 1..=steps {
        let x = from_x + (to_x - from_x) * i / steps;
        let y = from_y + (to_y - from_y) * i / steps;
        let (dx, dy) = abs_mouse_pos(x, y);
        send_mouse(dx, dy, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (ex, ey) = abs_mouse_pos(to_x, to_y);
    send_mouse(
        ex,
        ey,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTUP,
    );
    Ok(())
}

/// 拟人化点击前置：两段式移动逼近目标 + 停留让 hover 状态生效。
/// 真人点击是「移过去 → 停一下 → 再点」；瞬移后立刻按下，行的 hover 态/
/// 内部交互状态还没就绪，部分应用（汽水音乐等自渲染列表）会吞掉双击。
fn approach_and_hover(x: i32, y: i32) {
    let dx = if x > 24 { x - 18 } else { x };
    let dy = if y > 24 { y - 12 } else { y };
    mouse_move_abs(dx, dy);
    std::thread::sleep(std::time::Duration::from_millis(24));
    mouse_move_abs(x, y);
    std::thread::sleep(std::time::Duration::from_millis(160));
}

/// 左键单击：逼近 + hover 停留 → 按下 → 停顿 → 释放，合并为绝对坐标事件以保证命中精度。
fn click_left(x: i32, y: i32) {
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE,
    };
    approach_and_hover(x, y);
    let (dx, dy) = abs_mouse_pos(x, y);
    send_mouse(
        dx,
        dy,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTDOWN,
    );
    std::thread::sleep(std::time::Duration::from_millis(35));
    send_mouse(
        dx,
        dy,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTUP,
    );
    std::thread::sleep(std::time::Duration::from_millis(40));
}

/// 右键单击（打开上下文菜单等）。
fn click_right(x: i32, y: i32) {
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    };
    approach_and_hover(x, y);
    let (dx, dy) = abs_mouse_pos(x, y);
    send_mouse(
        dx,
        dy,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_RIGHTDOWN,
    );
    std::thread::sleep(std::time::Duration::from_millis(35));
    send_mouse(
        dx,
        dy,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_RIGHTUP,
    );
    std::thread::sleep(std::time::Duration::from_millis(40));
}

/// 左键双击：逼近 + hover 停留 → 两次「按下-释放」紧连（间隔 90ms，
/// 远小于系统双击阈值），避免被识别为拖拽/两次独立单击。
fn double_click_left(x: i32, y: i32) {
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE,
    };
    approach_and_hover(x, y);
    let (dx, dy) = abs_mouse_pos(x, y);
    for i in 0..2 {
        send_mouse(
            dx,
            dy,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTDOWN,
        );
        std::thread::sleep(std::time::Duration::from_millis(35));
        send_mouse(
            dx,
            dy,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTUP,
        );
        if i == 0 {
            std::thread::sleep(std::time::Duration::from_millis(90));
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(40));
}

/// 中键单击：关闭标签页 / 新标签页打开链接等场景。
fn middle_click(x: i32, y: i32) {
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    };
    approach_and_hover(x, y);
    let (dx, dy) = abs_mouse_pos(x, y);
    send_mouse(
        dx,
        dy,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MIDDLEDOWN,
    );
    std::thread::sleep(std::time::Duration::from_millis(35));
    send_mouse(
        dx,
        dy,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MIDDLEUP,
    );
    std::thread::sleep(std::time::Duration::from_millis(40));
}

/// 滚轮滚动：clicks 为「齿感格数」，正值向上/向右，负值向下/向左（Windows 原生语义）。
/// 每格 = 系统设置的滚轮行数（默认 3 行），应用内常对应一屏的 1/3 左右。
fn wheel_scroll(clicks: i32, horizontal: bool) {
    use ::windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    };
    if clicks == 0 {
        return;
    }
    let flag = if horizontal { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: clicks,
                dwFlags: flag,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
    std::thread::sleep(std::time::Duration::from_millis(120));
}

/// 把人类可读的组合键转成 uiautomation 的 `{Ctrl}` 格式，如 "ctrl+s" → "{Ctrl}s"
fn normalize_keys(keys: &str) -> String {
    let mut out = String::new();
    for part in keys.split('+') {
        let p = part.trim().to_lowercase();
        match p.as_str() {
            "ctrl" | "control" => out.push_str("{Ctrl}"),
            "alt" => out.push_str("{Alt}"),
            "shift" => out.push_str("{Shift}"),
            "win" | "meta" => out.push_str("{Win}"),
            "enter" | "return" => out.push_str("{Enter}"),
            "tab" => out.push_str("{Tab}"),
            "esc" | "escape" => out.push_str("{Esc}"),
            "space" => out.push(' '),
            // uiautomation crate 的 VIRTUAL_KEYS 表中退格键名为 BACK（无 BACKSPACE 别名），
            // 输出 {Backspace} 会报 "Error Input Format"
            "backspace" | "bs" => out.push_str("{BACK}"),
            "delete" | "del" => out.push_str("{Delete}"),
            "insert" | "ins" => out.push_str("{Insert}"),
            "up" => out.push_str("{Up}"),
            "down" => out.push_str("{Down}"),
            "left" => out.push_str("{Left}"),
            "right" => out.push_str("{Right}"),
            "home" => out.push_str("{Home}"),
            "end" => out.push_str("{End}"),
            "page_up" | "pageup" => out.push_str("{PAGE_UP}"),
            "page_down" | "pagedown" => out.push_str("{PAGE_DOWN}"),
            "pause" | "break" => out.push_str("{Pause}"),
            "print" | "printscreen" => out.push_str("{Print}"),
            other => {
                // F1-F24 功能键
                let f_key = other
                    .strip_prefix('f')
                    .and_then(|n| n.parse::<u16>().ok())
                    .filter(|f| (1..=24).contains(f));
                match f_key {
                    Some(f) => out.push_str(&format!("{{F{f}}}")),
                    None => out.push_str(other),
                }
            }
        }
    }
    out
}

/// 把人类可读键名映射为 Windows 虚拟键码（供 key_down / key_up 使用）
fn str_to_vk(name: &str) -> Option<VIRTUAL_KEY> {
    use KeyboardAndMouse::*;
    let n = name.trim().to_ascii_lowercase();
    let vk = match n.as_str() {
        // 修饰键
        "ctrl" | "control" => VK_CONTROL,
        "lctrl" | "lcontrol" => VK_LCONTROL,
        "rctrl" | "rcontrol" => VK_RCONTROL,
        "alt" | "menu" => VK_MENU,
        "lalt" | "lmenu" => VK_LMENU,
        "ralt" | "rmenu" => VK_RMENU,
        "shift" => VK_SHIFT,
        "lshift" => VK_LSHIFT,
        "rshift" => VK_RSHIFT,
        "win" | "meta" | "lwin" | "lwindows" => VK_LWIN,
        "rwin" | "rwindows" => VK_RWIN,
        // 常用键
        "enter" | "return" => VK_RETURN,
        "tab" => VK_TAB,
        "esc" | "escape" => VK_ESCAPE,
        "space" => VK_SPACE,
        "backspace" | "back" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "insert" => VK_INSERT,
        "pause" | "break" => VK_PAUSE,
        "print" | "printscreen" | "prtsc" => VK_PRINT,
        "capslock" | "capital" => VK_CAPITAL,
        "numlock" => VK_NUMLOCK,
        // 导航键
        "up" => VK_UP,
        "down" => VK_DOWN,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "home" => VK_HOME,
        "end" => VK_END,
        "page_up" | "pageup" | "prior" => VK_PRIOR,
        "page_down" | "pagedown" | "next" => VK_NEXT,
        // 媒体键
        "volume_up" => VK_VOLUME_UP,
        "volume_down" => VK_VOLUME_DOWN,
        "volume_mute" | "mute" => VK_VOLUME_MUTE,
        "media_play_pause" | "media_play" | "play_pause" => VK_MEDIA_PLAY_PAUSE,
        "media_next" | "media_next_track" | "next_track" => VK_MEDIA_NEXT_TRACK,
        "media_prev" | "media_prev_track" | "prev_track" => VK_MEDIA_PREV_TRACK,
        "media_stop" | "media_stop_track" => VK_MEDIA_STOP,
        // 小键盘运算键
        "numpad_add" => VK_ADD,
        "numpad_subtract" => VK_SUBTRACT,
        "numpad_multiply" => VK_MULTIPLY,
        "numpad_divide" => VK_DIVIDE,
        "numpad_decimal" => VK_DECIMAL,
        _ => {
            // 单字母 / 单数字
            if n.len() == 1 {
                let c = n.as_bytes()[0];
                if c.is_ascii_digit() {
                    return Some(VIRTUAL_KEY(VK_0.0 + (c - b'0') as u16));
                }
                if c.is_ascii_lowercase() {
                    return Some(VIRTUAL_KEY(VK_A.0 + (c - b'a') as u16));
                }
            }
            // F1-F24
            if let Some(num) = n.strip_prefix('f') {
                if let Ok(f) = num.parse::<u16>() {
                    if (1..=24).contains(&f) {
                        return Some(VIRTUAL_KEY(VK_F1.0 + f - 1));
                    }
                }
            }
            // 小键盘数字 numpad0-numpad9
            if let Some(num) = n.strip_prefix("numpad") {
                if let Ok(f) = num.parse::<u16>() {
                    if f <= 9 {
                        return Some(VIRTUAL_KEY(VK_NUMPAD0.0 + f));
                    }
                }
            }
            return None;
        }
    };
    Some(vk)
}

/// 剪贴板 + Ctrl+V 粘贴文本（保存并恢复原剪贴板内容，避免副作用）
fn paste_via_clipboard(text: &str) -> Result<(), String> {
    use std::time::Duration;

    let mut clipboard = Clipboard::new().map_err(|e| format!("打开剪贴板失败: {e}"))?;
    let original = clipboard.get_text().ok();
    clipboard
        .set_text(text.to_string())
        .map_err(|e| format!("写入剪贴板失败: {e}"))?;

    // 模拟 Ctrl+V
    Keyboard::new()
        .send_keys("{Ctrl}v")
        .map_err(|e| format!("发送 Ctrl+V 失败: {e}"))?;
    // 等目标应用完成粘贴（太短会让「恢复原剪贴板」抢在应用读取之前，粘贴出旧内容）
    std::thread::sleep(Duration::from_millis(500));

    // 恢复原剪贴板：仅当期间没有别的程序改写过（仍是我们要粘贴的内容）才恢复，
    // 避免覆盖掉用户/其他工具刚放上去的新内容
    if let Some(orig) = original {
        if clipboard.get_text().ok().as_deref() == Some(text) {
            let _ = clipboard.set_text(orig);
        }
    }
    Ok(())
}

// ───────────────────── 另存为对话框原语 ─────────────────────

/// 操作「另存为/保存」对话框（Win11 记事本 Ctrl+S 行为不一致时的可靠保存通道）：
/// 找到对话框 → 确保前台 → 全选文件名 → 粘贴完整路径 → 回车 → 自动确认「替换」弹窗 → 轮询文件落盘。
/// 返回 (是否成功, 说明)
fn operate_save_dialog(path: &str) -> Result<(bool, String), String> {
    use std::time::Duration;

    // 1) 找另存为对话框：前台窗口优先，其次任意可见的 #32770 且标题含 保存/另存为/Save
    let find_dialog = || -> Option<HWND> {
        struct Ctx {
            hit: Option<HWND>,
        }
        unsafe extern "system" fn proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let ctx = &mut *(lparam.0 as *mut Ctx);
            if ctx.hit.is_some() {
                return BOOL(1);
            }
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            if window_class(hwnd) != "#32770" {
                return BOOL(1);
            }
            let title = window_title(hwnd).unwrap_or_default().to_lowercase();
            if title.contains("另存为")
                || title.contains("保存")
                || title.contains("save")
            {
                ctx.hit = Some(hwnd);
            }
            BOOL(1)
        }
        let mut ctx = Ctx { hit: None };
        unsafe {
            EnumWindows(Some(proc), LPARAM(&mut ctx as *mut Ctx as isize));
        }
        ctx.hit
    };

    let dlg = find_dialog().ok_or("未找到「另存为/保存」对话框（class #32770）")?;
    unsafe {
        let _ = SetForegroundWindow(dlg);
    }
    std::thread::sleep(Duration::from_millis(150));

    // 2) 全选文件名 → 粘贴完整路径（文件名框在对话框打开时默认持有焦点）
    Keyboard::new()
        .send_keys("{Ctrl}a")
        .map_err(|e| format!("全选文件名失败: {e}"))?;
    std::thread::sleep(Duration::from_millis(80));
    paste_via_clipboard(path).map_err(|e| format!("填入路径失败: {e}"))?;
    std::thread::sleep(Duration::from_millis(150));

    // 3) 回车触发保存
    Keyboard::new()
        .send_keys("{Enter}")
        .map_err(|e| format!("回车失败: {e}"))?;

    // 4) 「确认另存为/替换」弹窗自动确认（轮询 2.5s，出现即回车，默认按钮=是）
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(500));
        struct Ctx {
            hit: Option<HWND>,
        }
        unsafe extern "system" fn confirm_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let ctx = &mut *(lparam.0 as *mut Ctx);
            if ctx.hit.is_some() || !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            if window_class(hwnd) != "#32770" {
                return BOOL(1);
            }
            let title = window_title(hwnd).unwrap_or_default().to_lowercase();
            if title.contains("确认另存为")
                || title.contains("替换")
                || title.contains("confirm save")
            {
                ctx.hit = Some(hwnd);
            }
            BOOL(1)
        }
        let mut ctx = Ctx { hit: None };
        unsafe {
            EnumWindows(Some(confirm_proc), LPARAM(&mut ctx as *mut Ctx as isize));
        }
        if let Some(confirm) = ctx.hit {
            unsafe {
                let _ = SetForegroundWindow(confirm);
            }
            std::thread::sleep(Duration::from_millis(120));
            let _ = Keyboard::new().send_keys("{Enter}"); // 默认按钮 = 是(Y)
        }
        // 5) 校验文件已落盘
        if std::path::Path::new(path).exists() {
            return Ok((
                true,
                format!("已通过另存为对话框保存到 {path}（含替换确认自动处理）"),
            ));
        }
    }

    if std::path::Path::new(path).exists() {
        Ok((true, format!("已通过另存为对话框保存到 {path}")))
    } else {
        Ok((
            false,
            format!(
                "对话框已操作但 {path} 未落盘：可能路径目录不存在或被应用弹窗拦截，建议截屏查看对话框当前状态"
            ),
        ))
    }
}

// ───────────────────── 游戏自动化原语（区域 OCR / 局面缓存 / 宏序列） ─────────────────────

/// 区域 OCR：截区域所在显示器 → 裁剪 → 识别，返回 (合并文本, 词数组绝对屏幕坐标)。
/// 回合制游戏局面感知的基础原语：只识别棋盘/商店/血条等固定小区域，比全屏快且噪音少
pub fn region_ocr_impl(x: f64, y: f64, w: f64, h: f64) -> Result<(String, Vec<Value>), String> {
    let (rx, ry, rw, rh) = (
        x.round() as i32,
        y.round() as i32,
        w.round() as i32,
        h.round() as i32,
    );
    if rw <= 0 || rh <= 0 {
        return Err("区域宽高必须为正".into());
    }
    let monitors = xcap::Monitor::all().map_err(|e| format!("枚举显示器失败: {e}"))?;
    let monitor = monitors
        .iter()
        .find(|m| {
            let (mx, my) = (m.x().unwrap_or(0), m.y().unwrap_or(0));
            let (mw, mh) = (m.width().unwrap_or(0) as i32, m.height().unwrap_or(0) as i32);
            rx >= mx && rx < mx + mw && ry >= my && ry < my + mh
        })
        .or_else(|| monitors.first())
        .ok_or("未找到显示器")?;
    let (mox, moy) = (monitor.x().unwrap_or(0), monitor.y().unwrap_or(0));
    let img = monitor.capture_image().map_err(|e| format!("截屏失败: {e}"))?;
    // 区域 → 截图像素坐标（钳制边界）
    let ix = (rx - mox).clamp(0, img.width() as i32 - 1) as u32;
    let iy = (ry - moy).clamp(0, img.height() as i32 - 1) as u32;
    let iw = rw.min(img.width() as i32 - ix as i32).max(1) as u32;
    let ih = rh.min(img.height() as i32 - iy as i32).max(1) as u32;
    let crop = image::imageops::crop(&mut { img }, ix, iy, iw, ih).to_image();

    let dir = std::env::temp_dir().join("baize-region");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;
    let crop_path = dir.join("crop.png");
    crop.save(&crop_path).map_err(|e| format!("保存裁剪图失败: {e}"))?;

    let (text, words) = crate::ocr::ocr_detect_gui(&crop_path.to_string_lossy())
        .map_err(|e| format!("区域识别失败: {e}"))?;
    // 词坐标（相对裁剪图）→ 绝对屏幕坐标（可直接用于 mouse_click）
    let abs_words: Vec<Value> = words
        .into_iter()
        .map(|mut w| {
            let wx = w["x"].as_f64().unwrap_or(0.0) + ix as f64 + mox as f64;
            let wy = w["y"].as_f64().unwrap_or(0.0) + iy as f64 + moy as f64;
            w["x"] = json!(wx);
            w["y"] = json!(wy);
            w
        })
        .collect();
    Ok((text, abs_words))
}

/// 局面缓存增量 diff：按 key 读写 %TEMP%\baize-board\{key}.json 快照，
/// 每次调用对全部命名区域 OCR，与上次快照对比返回 changed/unchanged。
/// 回合制游戏下一回合只看变化项，不再全量重新决策
pub fn board_diff_impl(key: &str, regions: &Value) -> Result<Value, String> {
    let arr = regions.as_array().ok_or("regions 必须为数组")?;
    if arr.is_empty() {
        return Err("regions 不能为空".into());
    }
    if arr.len() > 12 {
        return Err("regions 最多 12 个区域（太多请拆分）".into());
    }
    // 快照路径：key 清洗掉文件系统非法字符
    let safe: String = key
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    if safe.trim().is_empty() {
        return Err("key 不能为空".into());
    }

    let dir = std::env::temp_dir().join("baize-board");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建缓存目录失败: {e}"))?;
    let snap_path = dir.join(format!("{safe}.json"));
    let prev: Value = std::fs::read_to_string(&snap_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);

    let mut current = serde_json::Map::new();
    let mut regions_out: Vec<Value> = Vec::new();
    for r in arr {
        let name = r["name"]
            .as_str()
            .ok_or("每个 region 必须有 name")?
            .to_string();
        let (text, words) = region_ocr_impl(
            r["x"].as_f64().unwrap_or(0.0),
            r["y"].as_f64().unwrap_or(0.0),
            r["w"].as_f64().unwrap_or(0.0),
            r["h"].as_f64().unwrap_or(0.0),
        )?;
        current.insert(name.clone(), json!(text));
        regions_out.push(json!({ "name": name, "text": text, "words": words }));
    }

    let mut changed: Vec<Value> = Vec::new();
    let mut unchanged: Vec<Value> = Vec::new();
    for (name, text) in &current {
        let prev_text = prev["regions"][name].as_str().unwrap_or("");
        if prev_text == *text {
            unchanged.push(json!({ "name": name, "text": text }));
        } else {
            changed.push(json!({ "name": name, "text": text, "prev": prev_text }));
        }
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let snapshot = json!({ "ts": ts, "regions": current });
    std::fs::write(&snap_path, snapshot.to_string())
        .map_err(|e| format!("写入快照失败: {e}"))?;

    Ok(json!({
        "key": safe,
        "first_scan": prev.is_null(),
        "changed": changed,
        "unchanged": unchanged,
        "regions": regions_out,
        "note": "changed=与本回合上次快照不同的区域（重点决策依据）；unchanged=未变化可跳过",
    }))
}

/// 宏序列：按键/点击/等待按顺序批量执行——把「截屏-决策-多点几次」压成一次调用。
/// steps 每项：{action:"key",keys} | {action:"click"|"double_click"|"right_click",x,y} |
///            {action:"wait",ms} | {action:"type",text(ASCII)}
/// 上限 30 步 / 总时长 60s，游戏注入沿用拟人化点击
pub fn macro_impl(steps: &Value) -> Result<Value, String> {
    let arr = steps.as_array().ok_or("steps 必须为数组")?;
    if arr.is_empty() {
        return Err("steps 不能为空".into());
    }
    if arr.len() > 30 {
        return Err("steps 最多 30 步（更多请拆成多次调用）".into());
    }
    let t0 = std::time::Instant::now();
    let mut executed: Vec<String> = Vec::new();
    for (i, s) in arr.iter().enumerate() {
        if t0.elapsed().as_millis() > 60_000 {
            return Err(format!("宏执行超过 60s，在第 {} 步中止", i + 1));
        }
        let action = s["action"]
            .as_str()
            .ok_or_else(|| format!("第 {} 步缺少 action", i + 1))?;
        match action {
            "key" => {
                let keys = s["keys"]
                    .as_str()
                    .ok_or_else(|| format!("第 {} 步缺少 keys", i + 1))?;
                Keyboard::new()
                    .send_keys(&normalize_keys(keys))
                    .map_err(|e| format!("第 {} 步按键失败: {e}", i + 1))?;
                executed.push(format!("key:{keys}"));
            }
            "click" | "double_click" | "right_click" => {
                let x = s["x"].as_f64().ok_or_else(|| format!("第 {} 步缺少 x", i + 1))? as i32;
                let y = s["y"].as_f64().ok_or_else(|| format!("第 {} 步缺少 y", i + 1))? as i32;
                match action {
                    "click" => click_left(x, y),
                    "double_click" => double_click_left(x, y),
                    _ => click_right(x, y),
                }
                executed.push(format!("{action}:({x},{y})"));
            }
            "wait" => {
                let ms = s["ms"].as_u64().unwrap_or(300).clamp(50, 5000);
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            "type" => {
                let text = s["text"]
                    .as_str()
                    .ok_or_else(|| format!("第 {} 步缺少 text", i + 1))?;
                Keyboard::new()
                    .send_keys(text)
                    .map_err(|e| format!("第 {} 步输入失败: {e}", i + 1))?;
                executed.push(format!("type:{text}"));
            }
            other => {
                return Err(format!(
                    "第 {} 步未知 action「{other}」（支持 key/click/double_click/right_click/wait/type）",
                    i + 1
                ));
            }
        }
    }
    Ok(json!({
        "ok": true,
        "executed": executed,
        "duration_ms": t0.elapsed().as_millis() as u64,
    }))
}

// ───────────────────── UI 稳定性感知（知道界面什么时候稳定） ─────────────────────

/// UI 稳定性检测：间隔截屏缩成 16×16 灰度指纹，连续两次对比几乎一致即认为界面安定。
/// 用于「窗口刚打开/页面切换后等动画结束再定位」，解决坐标漂移与「点了没反应」的误判。
/// 返回 (是否安定, 实际等待毫秒)
pub fn wait_ui_stable(timeout_ms: u64) -> (bool, u64) {
    let start = std::time::Instant::now();
    let elapsed = || start.elapsed().as_millis() as u64;
    // 截光标所在显示器 → 16×16 灰度指纹（256 字节，抗噪且足够感知动画/转场）
    let snap = || -> Option<[u8; 256]> {
        let monitors = xcap::Monitor::all().ok()?;
        let monitor = monitors
            .iter()
            .find(|m| {
                let (mx, my) = (m.x().unwrap_or(0), m.y().unwrap_or(0));
                let (mw, mh) =
                    (m.width().unwrap_or(0) as i32, m.height().unwrap_or(0) as i32);
                let (cx, cy) = cursor_pos();
                cx >= mx && cx < mx + mw && cy >= my && cy < my + mh
            })
            .or_else(|| monitors.first())?;
        let img = monitor.capture_image().ok()?;
        let small =
            image::imageops::resize(&img, 16, 16, image::imageops::FilterType::Triangle);
        let mut out = [0u8; 256];
        for (i, p) in small.pixels().enumerate() {
            if i < 256 {
                out[i] = ((p[0] as u16 + p[1] as u16 + p[2] as u16) / 3) as u8;
            }
        }
        Some(out)
    };

    let mut prev = match snap() {
        Some(s) => s,
        None => return (false, elapsed()),
    };
    let mut stable_hits = 0u8;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if elapsed() >= timeout_ms {
            return (false, elapsed());
        }
        let cur = match snap() {
            Some(s) => s,
            None => return (false, elapsed()),
        };
        let diff: u32 = prev
            .iter()
            .zip(cur.iter())
            .map(|(a, b)| a.abs_diff(*b) as u32)
            .sum();
        prev = cur;
        // 指纹总差 <120（均 0.5 灰阶/点）≈ 无可见动画；连续 2 次达标才安定
        if diff < 120 {
            stable_hits += 1;
            if stable_hits >= 2 {
                return (true, elapsed());
            }
        } else {
            stable_hits = 0;
        }
    }
}

// ───────────────────── 窗口控制（防遮挡） ─────────────────────

/// 窗口标题（空标题返回空串，表示合法但无标题）
fn window_title(hwnd: HWND) -> Option<String> {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len < 0 {
            return None;
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let written = GetWindowTextW(hwnd, &mut buf);
        if written <= 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..written as usize]))
    }
}

/// 窗口类名（如 "#32770" 对话框；空标题弹窗据此识别）
fn window_class(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 256];
        let n = GetClassNameW(hwnd, &mut buf);
        if n <= 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&buf[..n as usize])
        }
    }
}

/// 窗口矩形（屏幕/虚拟桌面坐标）
fn window_rect(hwnd: HWND) -> Option<Rect> {
    unsafe {
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).as_bool() {
            Some(Rect {
                x: r.left as f64,
                y: r.top as f64,
                width: (r.right - r.left).max(0) as f64,
                height: (r.bottom - r.top).max(0) as f64,
            })
        } else {
            None
        }
    }
}

/// 窗口枚举上下文（list_windows 用）
struct WindowEnumCtx {
    wins: Vec<WindowInfo>,
}

unsafe extern "system" fn window_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut WindowEnumCtx);
    // 保留所有可见窗口；最小化的后台窗口不再过滤（曾导致「应用明明开着却找不到」），
    // 以 minimized=true 标记列出（聚焦时会自动 SW_RESTORE）
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    let name = window_title(hwnd).unwrap_or_default();
    let class = window_class(hwnd);
    // 连类名都没有的窗口（无意义/系统占位）跳过
    if name.is_empty() && class.is_empty() {
        return BOOL(1);
    }
    let iconic = IsIconic(hwnd).as_bool();
    let role = if class == "#32770" || class == "#32769" {
        "Dialog"
    } else {
        "Window"
    };
    // 最小化窗口的 GetWindowRect 是 (-32000,-32000) 占位值，置 None 避免误导
    let bbox = if iconic { None } else { window_rect(hwnd) };
    ctx.wins.push(WindowInfo {
        name,
        role: role.to_string(),
        class,
        bbox,
        minimized: iconic,
        process: window_process_name(hwnd).unwrap_or_default(),
    });
    BOOL(1)
}

/// 最小化遍历上下文
struct MinimizeCtx {
    except: Vec<String>,
    minimized: Vec<String>,
}

unsafe extern "system" fn minimize_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut MinimizeCtx);
    // 跳过不可见窗口与拥有者窗口（属于其他窗口的工具窗/弹出窗）
    if !IsWindowVisible(hwnd).as_bool() || GetWindow(hwnd, GW_OWNER).0 != 0 {
        return BOOL(1);
    }
    let title = window_title(hwnd).unwrap_or_default();
    if title.is_empty() {
        return BOOL(1);
    }
    let keep = ctx.except.iter().any(|e| {
        let e = e.trim();
        !e.is_empty() && title.to_lowercase().contains(&e.to_lowercase())
    });
    if keep {
        return BOOL(1);
    }
    ShowWindow(hwnd, SW_MINIMIZE);
    ctx.minimized.push(title);
    BOOL(1)
}

/// 按标题找窗口遍历上下文
struct FindCtx {
    name: String,
    found: Option<HWND>,
    /// 进程名兜底命中（标题不含关键词但进程 exe 名含，如「汽水」→ QishuiMusic.exe）
    process_hit: Option<HWND>,
}

unsafe extern "system" fn find_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut FindCtx);
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    let title = window_title(hwnd).unwrap_or_default();
    if !title.is_empty() && title.to_lowercase().contains(&ctx.name.to_lowercase()) {
        ctx.found = Some(hwnd);
        return BOOL(0); // 找到即停止
    }
    // 兜底：进程 exe 名含关键词（自绘/Electron 应用标题常常不含中文产品名）
    if ctx.process_hit.is_none() {
        if let Some(proc_name) = window_process_name(hwnd) {
            if proc_name.to_lowercase().contains(&ctx.name.to_lowercase()) {
                ctx.process_hit = Some(hwnd);
            }
        }
    }
    BOOL(1)
}

/// 窗口所属进程的 exe 文件名（如 "QishuiMusic.exe"；自绘应用标题不含关键词时靠进程名定位）
fn window_process_name(hwnd: HWND) -> Option<String> {
    use ::windows::Win32::Foundation::CloseHandle;
    use ::windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let full = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            ::windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .as_bool();
        let _ = CloseHandle(handle);
        if !full || len == 0 {
            return None;
        }
        // 取路径最后一段作为 exe 名
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().map(|s| s.to_string())
    }
}

/// 按标题关键词返回第一个匹配的顶层窗口句柄
fn find_window_by_name(name: &str) -> Option<HWND> {
    let mut ctx = FindCtx {
        name: name.trim().to_string(),
        found: None,
        process_hit: None,
    };
    if ctx.name.is_empty() {
        return None;
    }
    unsafe {
        EnumWindows(
            Some(find_enum_proc),
            LPARAM(&mut ctx as *mut FindCtx as isize),
        );
    }
    // 标题命中优先，进程名命中兜底
    ctx.found.or(ctx.process_hit)
}

/// 置顶 / 取消置顶指定窗口（带验证：确认 WS_EX_TOPMOST 位生效）
fn set_topmost_by_name(name: &str, topmost: bool) -> bool {
    match find_window_by_name(name) {
        Some(hwnd) => set_topmost_hwnd(hwnd, topmost),
        None => false,
    }
}

/// 置顶 / 取消置顶指定句柄（带验证：SetWindowPos 后读回 WS_EX_TOPMOST，不生效再重试一次）
fn set_topmost_hwnd(hwnd: HWND, topmost: bool) -> bool {
    unsafe {
        let insert_after = if topmost { HWND_TOPMOST } else { HWND_NOTOPMOST };
        for _ in 0..2 {
            let _ = SetWindowPos(hwnd, insert_after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            std::thread::sleep(std::time::Duration::from_millis(60));
            if is_topmost(hwnd) == topmost {
                return true;
            }
        }
        is_topmost(hwnd) == topmost
    }
}

/// 读取窗口的 WS_EX_TOPMOST 状态
fn is_topmost(hwnd: HWND) -> bool {
    unsafe { (GetWindowLongW(hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as i32) != 0 }
}

/// 当前前台窗口标题（聚焦验证失败时回报给模型，便于定位是谁抢了焦点）
fn foreground_title() -> String {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0 == 0 {
            return String::new();
        }
        window_title(fg).unwrap_or_default()
    }
}

/// 聚焦窗口并验证前台切换结果：SetForegroundWindow 受系统前台锁（后台进程无权抢焦点）时，
/// 先轻敲一次 Alt 解锁再重试，最多 3 轮，每轮 120ms 后校验 GetForegroundWindow。
fn focus_window_verified(hwnd: HWND) -> bool {
    use ::windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU;
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            // 还原动画/布局需要一点时间，立刻抢焦点容易失败
            std::thread::sleep(std::time::Duration::from_millis(180));
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        for _ in 0..3 {
            // 轻敲 Alt：解除 Windows 前台锁（注入事件，无副作用）
            keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYDOWN, 0);
            keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
            let _ = SetForegroundWindow(hwnd);
            std::thread::sleep(std::time::Duration::from_millis(120));
            if GetForegroundWindow() == hwnd {
                return true;
            }
        }
        GetForegroundWindow() == hwnd
    }
}

/// 一键清屏准备的最小化遍历上下文
struct PrepareCtx {
    self_pid: u32,
    keep: HWND,
    minimized: usize,
}

/// 清屏枚举：最小化除「目标窗口 / 本进程窗口 / 有主子的弹窗」外的所有可见顶层窗口
unsafe extern "system" fn prepare_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut PrepareCtx);
    if !IsWindowVisible(hwnd).as_bool() || hwnd == ctx.keep {
        return BOOL(1);
    }
    // 有拥有者的窗口是弹窗/工具窗，跟随主窗口，不单独处理
    if GetWindow(hwnd, GW_OWNER).0 != 0 {
        return BOOL(1);
    }
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == ctx.self_pid {
        return BOOL(1);
    }
    if window_title(hwnd).unwrap_or_default().is_empty() {
        return BOOL(1);
    }
    let _ = ShowWindow(hwnd, SW_MINIMIZE);
    ctx.minimized += 1;
    BOOL(1)
}

/// 聚焦并前置指定窗口（最小化则先还原）
/// 按标题查找可见顶层窗口的物理屏幕矩形 [x, y, w, h]（供光圈环绕目标应用）
pub fn find_window_rect(title: &str) -> Option<[i32; 4]> {
    let hwnd = find_window_by_name(title)?;
    unsafe {
        let mut r = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut r).as_bool() {
            return Some([
                r.left,
                r.top,
                (r.right - r.left).max(1),
                (r.bottom - r.top).max(1),
            ]);
        }
    }
    None
}

/// 当前鼠标物理坐标（点击注入后光标即落在目标上，供光环闪烁定位）
pub fn cursor_pos() -> (i32, i32) {
    let mut pt = unsafe { std::mem::zeroed() };
    unsafe {
        GetCursorPos(&mut pt);
    }
    (pt.x, pt.y)
}

fn focus_window_by_name(name: &str) -> bool {
    match find_window_by_name(name) {
        Some(hwnd) => focus_window_verified(hwnd),
        None => false,
    }
}
