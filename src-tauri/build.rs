fn main() {
    // Windows：重编译链接前先结束仍在运行的调试版实例。
    // 运行中的 baize.exe 被系统锁定，链接器无法替换它，会报
    // 「failed to remove file ... baize.exe · 拒绝访问 (os error 5)」。
    #[cfg(windows)]
    kill_stale_debug_instance();
    #[cfg(windows)]
    delayload_comctl32();
    tauri_build::build()
}

/// 把 comctl32 改成**延迟加载**，修掉 `cargo test` 启动即 `0xC0000139` 的问题。
///
/// 背景（2026-10-17 定位，PE 导入表逐符号核对）：
/// 依赖树（`tauri-runtime-wry` / `muda` / `rfd`）会从 comctl32 导入
/// `TaskDialogIndirect`，这个符号只存在于 comctl32 **v6**（WinSxS 重定向）；
/// `C:\Windows\System32\comctl32.dll` 是 5.82，没有该导出。
/// Tauri 只给**主 bin** 嵌带 Common-Controls v6 的清单，所以：
///   - `cargo build` 出的 baize.exe 正常；
///   - lib 的单元测试二进制、examples 没有清单 → 加载器绑定 5.82 →
///     启动即 `STATUS_ENTRYPOINT_NOT_FOUND (0xC0000139)`。
/// 这就是「`cargo test` 一直跑不起来」的真正原因，不是代码错、不是环境缺 DLL。
///
/// 为什么不用「给测试目标补清单」：cargo 的 `rustc-link-arg-tests` 只作用于
/// `tests/` 目录（本项目没有），而无作用域的 `rustc-link-arg` 会连主 bin 一起加，
/// 与 tauri 已嵌入的 RT_MANIFEST 冲突（实测 `CVT1100: 资源重复, 类型: MANIFEST`）。
/// 延迟加载没有这个问题：导入不再在加载期解析，进程能正常起来；
/// 真正调用该符号时才解析（单元测试不会调用对话框，主 bin 有清单照常解析到 v6）。
#[cfg(windows)]
fn delayload_comctl32() {
    println!("cargo:rustc-link-arg=/DELAYLOAD:comctl32.dll");
    println!("cargo:rustc-link-lib=delayimp");
}

/// 只结束 target\debug 目录下的实例（tauri dev / cargo build 产物），
/// 不碰用户安装的正式版。
///
/// 注意（2026-10-17 实测）：这里**不能**再派生常驻守护进程。历史上为了
/// 「链接期间 dev watcher 又拉起实例」，曾 spawn 一个每 0.5s 查杀、持续 10 分钟的
/// PowerShell 守护。实测后果：
///   - cargo 构建在 `Compiling baize` 之后卡住 8 分钟以上（build script 的 unit
///     一直不结束，rustc 迟迟不启动）；把守护去掉后同一构建 37 秒就进入 rustc。
///   - 反复构建会累积多个残留守护（杀 cargo 带不走它们），越编越慢。
/// 因此改为「只在调试版实例确实存在时同步查杀一次」。链接期被重新拉起的窗口很短，
/// 且 dev watcher 在 cargo 持锁期间本就不会重启，一次查杀已足够。
#[cfg(windows)]
fn kill_stale_debug_instance() {
    let exe = std::env::current_dir()
        .map(|d| d.join("target").join("debug").join("baize.exe"))
        .unwrap_or_default();

    // 没有调试版实例就直接返回：省掉一次约 4 秒的 PowerShell 启动。
    if !exe.exists() {
        return;
    }
    let _ = kill_once();
}

#[cfg(windows)]
fn kill_once() -> std::io::Result<()> {
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-Process baize -ErrorAction SilentlyContinue | \
             Where-Object { $_.Path -like '*\\target\\debug\\*' } | Stop-Process -Force",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .map(|_| ())
}
