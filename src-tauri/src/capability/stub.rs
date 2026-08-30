//! 非 Windows 平台的占位实现（M2 后续接入 macOS AX / Linux AT-SPI）

use super::*;

pub struct StubCapability;

impl Capability for StubCapability {
    fn probe(&self) -> CapabilitySet {
        CapabilitySet {
            a11y: false,
            screenshot: false,
            input: false,
        }
    }

    fn list_windows(&self) -> Result<Vec<WindowInfo>, CapError> {
        Err(CapError::Unsupported(
            "当前平台暂未实现窗口枚举（M2 后续接入 macOS AX / Linux AT-SPI）",
        ))
    }

    fn observe(&self, _req: &ObserveReq) -> Result<Observation, CapError> {
        Err(CapError::Unsupported(
            "当前平台暂未实现无障碍树读取（M2 后续接入 macOS AX / Linux AT-SPI）",
        ))
    }

    fn capture_screen(&self) -> Result<ScreenshotInfo, CapError> {
        Err(CapError::Unsupported("当前平台暂未实现截屏"))
    }

    fn act(&self, action: &Action) -> Result<ActionResult, CapError> {
        match action {
            Action::ReadOnly => Ok(ActionResult {
                ok: true,
                description: "只读".to_string(),
            }),
            _ => Err(CapError::Unsupported("当前平台暂未实现输入注入")),
        }
    }

    fn find(&self, _target: &str) -> Result<Vec<ElementMatch>, CapError> {
        Err(CapError::Unsupported("当前平台暂未实现语义定位"))
    }

    fn find_anywhere(&self, _target: &str) -> Result<Vec<ElementMatch>, CapError> {
        Err(CapError::Unsupported("当前平台暂未实现语义定位"))
    }

    fn click_element(&self, _target: &str) -> Result<ActionResult, CapError> {
        Err(CapError::Unsupported("当前平台暂未实现语义点击"))
    }
}
