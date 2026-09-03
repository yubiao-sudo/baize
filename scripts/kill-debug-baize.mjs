// 结束仍在运行的「调试版」白泽（target\debug 下的 baize.exe）。
// 运行中的 exe 会被 Windows 锁定，链接器无法替换 →
// 「failed to remove file ... baize.exe · 拒绝访问 (os error 5)」。
// 只按进程路径过滤 target\debug 目录，不影响用户安装的正式版。
// 由 tauri.conf.json 的 beforeDevCommand / beforeBuildCommand 在每次构建前确定性调用。
import { execSync, spawn } from "node:child_process";

const PS_KILL =
  "Get-Process baize -ErrorAction SilentlyContinue | " +
  "Where-Object { $_.Path -like '*\\target\\debug\\*' } | Stop-Process -Force";

function killOnce() {
  try {
    execSync(`powershell -NoProfile -Command "${PS_KILL}"`, { stdio: "pipe" });
    return true;
  } catch {
    // 无实例 / 权限不足时静默跳过
    return false;
  }
}

killOnce();
console.log("[pre-build] 已清理残留的调试版白泽实例（如有）");

// 构建期间应用可能被重新拉起，会在链接阶段再次锁死 exe：
// 派生一个 10 分钟的后台守护，每 0.5s 查杀「早于本守护启动的旧实例」。
// 关键：按进程启动时间过滤（StartTime < 守护启动时刻），
// 这样「无需重编译、cargo 秒拉起新实例」时新实例启动时间晚于守护，不会被误杀。
// （旧逻辑按 exe mtime 是否变化来决定退出，在无需重编译时会一直误杀刚启动的实例。）
const watcher = `
$baseline=(Get-Date); $end=$baseline.AddMinutes(10)
while((Get-Date) -lt $end) {
  Get-Process baize -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -like '*\\target\\debug\\*' -and $_.StartTime -lt $baseline } |
    Stop-Process -Force
  Start-Sleep -Milliseconds 500
}`;

try {
  spawn("powershell", ["-NoProfile", "-Command", watcher], {
    detached: true,
    stdio: "ignore",
  }).unref();
} catch {
  // 守护失败不影响构建，构建前的确定性查杀已覆盖绝大多数场景
}
