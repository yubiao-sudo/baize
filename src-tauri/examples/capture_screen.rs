//! 独立验证：xcap 截屏（与 capability/windows.rs 的 capture_screen 同逻辑）
//! 运行：cargo run --example capture_screen

use xcap::Monitor;

fn main() {
    let monitors = Monitor::all().expect("枚举显示器失败");
    println!("显示器数量: {}", monitors.len());

    let m = &monitors[0];

    let img = m.capture_image().expect("截屏失败");
    println!("截图尺寸: {}x{}", img.width(), img.height());

    let path = "baize-screenshot-test.png";
    img.save(path).expect("保存 PNG 失败");
    let abs = std::env::current_dir().unwrap().join(path);
    println!("已保存: {}", abs.display());
    println!("文件大小: {} bytes", std::fs::metadata(&abs).unwrap().len());
}
