//! GUI 自动化实测探针（开发用，不随安装包分发——release 构建前可删除或保留）。
//!
//! 对指定窗口依次执行：app_profile → observe → interactive_map → find →（可选）click_element，
//! 每步打印耗时，用于验证：
//!   1. 非自绘应用（记事本/计算器）：UIA 树 rich，全链路秒级返回
//!   2. 自绘/Electron 应用（汽水音乐）：刚启动首调 interactive_map 应在 8s 超时返回
//!      （而不是挂死数分钟），等 1-2s 重试应成功读到控件
//!
//! 用法：cargo run --bin gui_probe -- <窗口关键词> [--find 关键词] [--click 按钮名]
//! 例： cargo run --bin gui_probe -- 记事本 --find 文本编辑器
//!     cargo run --bin gui_probe -- 汽水音乐 --click 播放

use baize_lib::capability::{create_capability, ObserveReq};
use std::time::Instant;

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let target = args.get(1).cloned().unwrap_or_default();
    let find_kw = arg_after(&args, "--find");
    let click_target = arg_after(&args, "--click");
    let cap = create_capability();
    let win = if target.is_empty() { None } else { Some(target.clone()) };

    println!("===== GUI 探针：target={target:?} =====");

    // --list：枚举窗口后退出（诊断用）
    if args.iter().any(|a| a == "--list") {
        match cap.list_windows() {
            Ok(wins) => {
                for w in &wins {
                    println!(
                        "  [{:?}] {:<28} class={:<24} process={:<20} min={}",
                        w.role, w.name, w.class, w.process, w.minimized
                    );
                }
                println!("共 {} 个窗口", wins.len());
            }
            Err(e) => println!("list_windows ERROR: {e}"),
        }
        return;
    }

    // 0. 应用类型画像
    let t = Instant::now();
    match baize_lib::capability::app_profile_for_probe(win.as_deref()) {
        Ok(v) => println!(
            "[app_profile  {:>9?}] type={} quality={} nodes={} suggest={:?}",
            t.elapsed(),
            v["app_type"],
            v["a11y_quality"],
            v["node_count"],
            v["suggest"].as_array().map(|a| a.len()).unwrap_or(0)
        ),
        Err(e) => println!("[app_profile  {:>9?}] ERROR: {e}", t.elapsed()),
    }

    // 1. observe：全树构建（超时保护 12s）
    let t = Instant::now();
    match cap.observe(&ObserveReq::default()) {
        Ok(obs) => println!(
            "[observe      {:>9?}] node_count={} truncated={}",
            t.elapsed(),
            obs.tree.as_ref().map(|x| x.node_count).unwrap_or(0),
            obs.tree.as_ref().map(|x| x.truncated).unwrap_or(false)
        ),
        Err(e) => println!("[observe      {:>9?}] ERROR: {e}", t.elapsed()),
    }

    // 2. interactive_map：可交互元素地图（超时保护 8s）
    let t = Instant::now();
    match cap.interactive_map(win.clone()) {
        Ok(v) => println!(
            "[interactive  {:>9?}] count={} note={}",
            t.elapsed(),
            v["count"],
            v["note"].as_str().unwrap_or("-")
        ),
        Err(e) => println!("[interactive  {:>9?}] ERROR: {e}", t.elapsed()),
    }

    // 3. find：控件搜索（跨窗口，超时保护 8s）
    if let Some(kw) = find_kw {
        let t = Instant::now();
        match cap.find(&kw) {
            Ok(m) => println!(
                "[find         {:>9?}] matches={} top={:?}",
                t.elapsed(),
                m.len(),
                m.first().map(|x| x.name.clone())
            ),
            Err(e) => println!("[find         {:>9?}] ERROR: {e}", t.elapsed()),
        }
    }

    // 4. click_element：语义点击（UIA 段超时 8s 后落视觉兜底）
    if let Some(ct) = click_target {
        let t = Instant::now();
        match cap.click_element(&ct) {
            Ok(r) => println!("[click_element {:>8?}] {}", t.elapsed(), r.description),
            Err(e) => println!("[click_element {:>8?}] ERROR: {e}", t.elapsed()),
        }
    }

    println!("===== 完成 =====");
}
