//! 弹窗处理：检测并关闭第三方应用启动/运行时的弹窗。
//!
//! 适用场景：广告弹窗、更新提示、欢迎引导、用户协议、错误提示、登录弹窗等。
//! 策略链（从精确到兜底）：
//!   1. UIA 无障碍树精确找关闭类控件 → 点击中心；
//!   2. 截图 + 本地 OCR 定位关闭类文字 → 点击中心；
//!   3. 按 Esc（安全兜底）；
//!   4. 按 Alt+F4（仅当显式 allow_altf4=true，默认不启用，避免误关前台应用）。

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::{Action, Capability};
use crate::tools::{PermissionClass, Tool};

/// 关闭类按钮关键词（点它让弹窗消失却不触发业务动作）
const CLOSE_KEYWORDS: &[&str] = &[
    "关闭窗口",
    "关闭",
    "稍后",
    "以后再说",
    "下次再说",
    "跳过",
    "取消",
    "忽略",
    "我知道了",
    "知道了",
    "不再提示",
    "关闭应用",
    "✕",
    "×",
];

/// 确认类按钮关键词（用户协议 / 登录等弹窗需要「同意/确定」才能继续）
const CONFIRM_KEYWORDS: &[&str] = &[
    "同意并继续",
    "同意",
    "接受",
    "继续",
    "开始使用",
    "立即体验",
    "下一步",
    "确定",
    "我已阅读",
];

/// 卸载/安装过程中的确认弹窗按钮关键词（「确定卸载/是/确定/继续」等推进操作）
/// 比 CONFIRM_KEYWORDS 更偏卸载语境，供 `confirm_dialogs` 在命令运行时自动点击。
const UNINSTALL_CONFIRM_KEYWORDS: &[&str] = &[
    "确定卸载",
    "确认卸载",
    "仍然卸载",
    "继续卸载",
    "卸载",
    "移除",
    "确定",
    "是",
    "是(&Y)",
    "Yes",
    "OK",
    "下一步",
    "下一步(&N)",
    "继续",
    "Next",
];

pub struct ClosePopupTool {
    capability: Arc<dyn Capability>,
}

impl ClosePopupTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for ClosePopupTool {
    fn name(&self) -> &str {
        "close_popup"
    }
    fn description(&self) -> &str {
        "检测并关闭当前屏幕上的弹窗/对话框（广告、更新提示、欢迎引导、错误提示等）。
         打开第三方应用后若出现弹窗可调用本工具：先尝试点击「关闭/稍后/跳过/取消」等按钮，
         失败则按 Esc（可选 Alt+F4）。mode 可选 close（点关闭按钮，默认）或 confirm（点「确定/同意」按钮）。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "description": "close=关闭弹窗（默认，点 关闭/稍后/跳过/取消 等）；confirm=确认弹窗（点 确定/同意/接受 等）"
                },
                "keywords": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "自定义要点击的按钮关键词（覆盖默认列表）"
                },
                "allow_altf4": {
                    "type": "boolean",
                    "description": "兜底是否允许按 Alt+F4 强制关闭前台窗口，默认 false（较危险，注意可能误关）"
                }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let mode = args["mode"].as_str().unwrap_or("close");
        let keywords: Vec<String> = if let Some(arr) = args["keywords"].as_array() {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        } else if mode == "confirm" {
            CONFIRM_KEYWORDS.iter().map(|s| s.to_string()).collect()
        } else {
            CLOSE_KEYWORDS.iter().map(|s| s.to_string()).collect()
        };

        // 1. UIA 无障碍树：精确找关闭/确认控件。
        // confirm 弹窗通常是独立的顶层对话框，用 find_anywhere 跨所有窗口搜索；
        // close 默认只查前台窗口，避免误点后台应用里的「关闭」按钮。
        let search_all = mode == "confirm";
        for kw in &keywords {
            let found = if search_all {
                self.capability.find_anywhere(kw)
            } else {
                self.capability.find(kw)
            };
            if let Ok(matches) = found {
                for m in &matches {
                    if let Some(b) = m.bbox {
                        if b.width > 2.0 && b.height > 2.0 {
                            let cx = b.x + b.width / 2.0;
                            let cy = b.y + b.height / 2.0;
                            self.capability
                                .act(&Action::ClickAt { x: cx, y: cy })
                                .map_err(|e| e.to_string())?;
                            return Ok(json!({
                                "closed": true,
                                "method": "uia",
                                "label": kw,
                                "pos": [cx, cy]
                            }));
                        }
                    }
                }
            }
        }

        // 2. 截图 + OCR：一次识别所有文字，找关闭/确认关键词
        if let Ok(info) = self.capability.capture_screen() {
            if let Ok((_, words)) = crate::ocr::ocr_detect_gui(&info.path) {
                for kw in &keywords {
                    if let Some(w) = words.iter().find(|w| {
                        let s = w["text"].as_str().unwrap_or("").trim();
                        !s.is_empty() && (s.contains(kw.as_str()) || kw.contains(s))
                    }) {
                        let x = w["x"].as_f64().unwrap_or(0.0) + info.offset_x as f64;
                        let y = w["y"].as_f64().unwrap_or(0.0) + info.offset_y as f64;
                        let ww = w["w"].as_f64().unwrap_or(0.0);
                        let hh = w["h"].as_f64().unwrap_or(0.0);
                        let cx = x + ww / 2.0;
                        let cy = y + hh / 2.0;
                        self.capability
                            .act(&Action::ClickAt { x: cx, y: cy })
                            .map_err(|e| e.to_string())?;
                        return Ok(json!({
                            "closed": true,
                            "method": "ocr",
                            "label": w["text"].clone(),
                            "pos": [cx, cy]
                        }));
                    }
                }
            }
        }

        // 3. Esc 兜底（安全：多数弹窗可用 Esc 关闭）
        self.capability
            .act(&Action::KeyPress { keys: "esc".to_string() })
            .map_err(|e| e.to_string())?;

        // 4. Alt+F4（仅显式允许，避免误关前台应用）
        if args["allow_altf4"].as_bool() == Some(true) {
            self.capability
                .act(&Action::KeyPress { keys: "alt+f4".to_string() })
                .map_err(|e| e.to_string())?;
            return Ok(json!({ "closed": true, "method": "altf4", "note": "未找到关闭按钮，已按 Alt+F4 关闭前台窗口" }));
        }

        Ok(json!({
            "closed": false,
            "method": "esc",
            "note": "未检测到可识别的关闭按钮，已尝试按 Esc；若仍有弹窗，可换 click_element 指定控件，或给 close_popup 传 keywords 参数"
        }))
    }
}

/// 自动点击卸载/安装过程中出现的确认类弹窗按钮，返回是否点到了什么。
/// 与 close_popup 的 confirm 模式不同：这里不做 Esc / Alt+F4 兜底（Esc 可能直接取消卸载器），
/// 且跨所有顶层窗口搜索，专供 software_install / software_uninstall 在命令运行期间轮询调用。
pub fn confirm_dialogs(capability: &dyn Capability) -> Value {
    // 策略 1：UIA 无障碍树跨所有顶层窗口找确认按钮（确定卸载/是/确定/继续/Next…）。
    for kw in UNINSTALL_CONFIRM_KEYWORDS {
        if let Ok(matches) = capability.find_anywhere(kw) {
            for m in &matches {
                if let Some(b) = m.bbox {
                    if b.width > 2.0 && b.height > 2.0 {
                        let cx = b.x + b.width / 2.0;
                        let cy = b.y + b.height / 2.0;
                        if capability
                            .act(&Action::ClickAt { x: cx, y: cy })
                            .is_ok()
                        {
                            return json!({
                                "clicked": true,
                                "method": "uia",
                                "label": m.name,
                                "pos": [cx, cy]
                            });
                        }
                    }
                }
            }
        }
    }

    // 策略 2：部分卸载程序使用自绘按钮，不通过 UIA 暴露，需截图 + OCR 定位文字再点。
    // 复用一张截图，逐关键词在 OCR 结果里命中一次点击一次。
    if let Ok(info) = capability.capture_screen() {
        if let Ok((_, words)) = crate::ocr::ocr_detect_gui(&info.path) {
            for kw in UNINSTALL_CONFIRM_KEYWORDS {
                let hit = words.iter().find(|w| {
                    let s = w["text"].as_str().unwrap_or("").trim();
                    !s.is_empty() && (s.contains(kw) || kw.contains(s))
                });
                if let Some(w) = hit {
                    let x = w["x"].as_f64().unwrap_or(0.0) + info.offset_x as f64;
                    let y = w["y"].as_f64().unwrap_or(0.0) + info.offset_y as f64;
                    let ww = w["w"].as_f64().unwrap_or(0.0);
                    let hh = w["h"].as_f64().unwrap_or(0.0);
                    let cx = x + ww / 2.0;
                    let cy = y + hh / 2.0;
                    if capability.act(&Action::ClickAt { x: cx, y: cy }).is_ok() {
                        return json!({
                            "clicked": true,
                            "method": "ocr",
                            "label": w["text"].clone(),
                            "pos": [cx, cy]
                        });
                    }
                }
            }
        }
    }
    json!({ "clicked": false })
}