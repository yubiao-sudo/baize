//! 独立验证：uiautomation 读屏（与 capability/windows.rs 同逻辑）
//! 运行：cargo run --example read_screen

use uiautomation::controls::ControlType;
use uiautomation::core::UICondition;
use uiautomation::types::TreeScope;
use uiautomation::{UIElement, UIAutomation};

fn main() {
    let automation = UIAutomation::new().expect("创建 UIAutomation 失败");
    let focused = automation.get_focused_element().expect("获取焦点元素失败");
    let condition = automation.create_true_condition().expect("创建条件失败");

    // 窗口枚举
    let desktop = automation.get_root_element().expect("获取桌面失败");
    println!("=== 顶层窗口 ===");
    if let Ok(children) = desktop.find_all(TreeScope::Children, &condition) {
        for c in children {
            let n = c.get_name().unwrap_or_default();
            if !n.is_empty() {
                println!("[{}] {}", format!("{:?}", c.get_control_type().unwrap()), n);
            }
        }
    }
    println!();

    // 向上回溯到顶层窗口
    let mut root = focused;
    if let Ok(ancestors) = root.find_all(TreeScope::Ancestors, &condition) {
        if let Some(win) = ancestors
            .into_iter()
            .find(|a| matches!(a.get_control_type(), Ok(ControlType::Window)))
        {
            root = win;
        }
    }

    println!(
        "根元素: [{}] name='{}'",
        format!("{:?}", root.get_control_type().unwrap()),
        root.get_name().unwrap_or_default()
    );

    let mut count = 0usize;
    walk(&root, 0, &condition, &mut count);
    println!("节点总数: {count}");
}

fn walk(el: &UIElement, depth: usize, condition: &UICondition, count: &mut usize) {
    if depth > 5 || *count >= 60 {
        return;
    }
    *count += 1;
    let name = el.get_name().unwrap_or_default();
    let role = format!("{:?}", el.get_control_type().unwrap());
    let bbox = el.get_bounding_rectangle().ok().map(|r| {
        format!(
            "({},{},{},{})",
            r.get_left(),
            r.get_top(),
            r.get_right(),
            r.get_bottom()
        )
    });
    println!(
        "{}[{}] name='{}' bbox={}",
        "  ".repeat(depth),
        role,
        name,
        bbox.unwrap_or_default()
    );
    if let Ok(children) = el.find_all(TreeScope::Children, condition) {
        for c in children {
            walk(&c, depth + 1, condition, count);
        }
    }
}
