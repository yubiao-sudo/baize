//! GUI 自动化实测入口（开发用）。
//!
//! ⚠️ 本文件必须放在 dev-probe/ 而不是 src/bin/：
//!    Cargo 会自动发现 src/bin/*.rs 为额外二进制，Tauri 打包器在多 bin 时
//!    会选错主程序（v0.8.0~v0.8.3 安装包因此装的是探针而非主程序，启动即闪退）。
//!    需要构建探针时：cargo build --release 时手动移回或用
//!    `rustc --edition 2021 dev-probe/gui_probe.rs` 之外的方式临时编译。
//!
//! 两种模式：
//! 1. 全链路探针：gui_probe <窗口关键词> [--find 关键词] [--click 按钮名] [--list]
//! 2. 工具调用（与白泽 agent 完全同构）：gui_probe tool <工具名> '<json 参数>'
//!    例：gui_probe tool window_focus '{"name":"记事本"}'
//!        gui_probe tool screen_elements '{"window":"记事本"}'
//!        gui_probe tool click_element '{"target":"文件"}'

use baize_lib::capability::{create_capability, dispatch_tool, ObserveReq};
use std::time::Instant;

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    // 崩溃取证先行：SEH 级未处理异常写 %TEMP%\baize-crash-<pid>-*.log
    baize_lib::capability::crashlog_install();
    // 探针进程必须 DPI-aware：否则 GetSystemMetrics/UIA bbox 返回逻辑坐标
    // （125% 缩放下 1536×864），与截图/SendInput 物理坐标系（1920×1080）分裂，
    // 点击系统性偏移 25%。白泽本体由 tao 运行时设置，探针需自行声明。
    #[cfg(windows)]
    unsafe {
        use ::windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let args: Vec<String> = std::env::args().collect();

    // ── 自验模式：故意段错误，验证 crashlog 落盘 ──
    if args.get(1).map(|s| s.as_str()) == Some("crashme") {
        println!("故意触发 0xC0000005……");
        unsafe {
            let p: *const u8 = std::ptr::null();
            std::ptr::read_volatile(p);
        }
        unreachable!();
    }

    // ── 工具调用模式：与白泽 agent 同一执行路径 ──
    if args.get(1).map(|s| s.as_str()) == Some("tool") {
        let tool_name = args
            .get(2)
            .cloned()
            .unwrap_or_else(|| "缺工具名".to_string());
        let json_str = args.get(3).cloned().unwrap_or_else(|| "{}".to_string());
        let parsed: serde_json::Value = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("JSON 参数解析失败: {e}\n原始: {json_str}");
                std::process::exit(2);
            }
        };
        let cap = create_capability();
        let t = Instant::now();
        match dispatch_tool(&cap, &tool_name, parsed) {
            Ok(v) => {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                eprintln!("[{tool_name} {:>9?}]", t.elapsed());
            }
            Err(e) => {
                eprintln!("[{tool_name} {:>9?}] ERROR: {e}", t.elapsed());
                std::process::exit(1);
            }
        }
        return;
    }

    // ── 压测模式：同进程循环跑生产 UIA 工作线程路径，隔离间歇性段错误 ──
    // 用法：gui_probe stress <locate|click|full> <目标名> <次数>
    //       gui_probe stress uia <窗口名> <目标名> <次数>   （ByName 根内 el.click()）
    if args.get(1).map(|s| s.as_str()) == Some("stress") {
        let mode = args.get(2).cloned().unwrap_or_default();
        let (target, n, uia_window) = if mode == "uia" {
            (
                args.get(4).cloned().unwrap_or_default(),
                args.get(5).and_then(|s| s.parse().ok()).unwrap_or(20),
                args.get(3).cloned().unwrap_or_default(),
            )
        } else {
            (
                args.get(3).cloned().unwrap_or_default(),
                args.get(4).and_then(|s| s.parse().ok()).unwrap_or(20),
                String::new(),
            )
        };
        use std::io::Write as _;
        // full 模式：完整生产路径（UIA 工作线程 + 视觉兜底链截图/OCR/接地）
        let full_cap = if mode == "full" {
            Some(create_capability())
        } else {
            None
        };
        for i in 0..n {
            if let (Some(cap), Some(tg)) = (&full_cap, args.get(3)) {
                let t = Instant::now();
                let outcome = match cap.click_element(tg) {
                    Ok(r) => format!("ok: {}", r.description),
                    Err(e) => format!("ERR: {e}"),
                };
                println!("iter {}/{} [{:>8?}] {}", i + 1, n, t.elapsed(), outcome);
                let _ = std::io::stdout().flush();
                continue;
            }
            let target_owned = target.clone();
            let mode_owned = mode.clone();
            let uia_window_owned = uia_window.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let t = Instant::now();
            std::thread::spawn(move || {
                let res = if mode_owned == "locate" {
                    baize_lib::capability::windows_for_probe_locate(&target_owned)
                } else if mode_owned == "uia" {
                    baize_lib::capability::windows_for_probe_uia_click(
                        &uia_window_owned,
                        &target_owned,
                    )
                } else {
                    baize_lib::capability::windows_for_probe_click(&target_owned)
                };
                let _ = tx.send(res);
            });
            let outcome = match rx.recv_timeout(std::time::Duration::from_millis(8000)) {
                Ok(Some(Ok(r))) => format!("ok: {}", r.description),
                Ok(Some(Err(e))) => format!("ERR: {e}"),
                Ok(None) => "no-hit".to_string(),
                Err(_) => "TIMEOUT(8s)".to_string(),
            };
            println!("iter {}/{} [{:>8?}] {}", i + 1, n, t.elapsed(), outcome);
            let _ = std::io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        println!("===== stress 完成 =====");
        return;
    }

    // ── 原全链路探针模式 ──
    let target = args.get(1).cloned().unwrap_or_default();
    let find_kw = arg_after(&args, "--find");
    let click_target = arg_after(&args, "--click");
    let cap = create_capability();
    let win = if target.is_empty() { None } else { Some(target.clone()) };

    println!("===== GUI 探针：target={target:?} =====");

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

    if let Some(ct) = click_target {
        let t = Instant::now();
        match cap.click_element(&ct) {
            Ok(r) => println!("[click_element {:>8?}] {}", t.elapsed(), r.description),
            Err(e) => println!("[click_element {:>8?}] ERROR: {e}", t.elapsed()),
        }
    }

    println!("===== 完成 =====");
}
