// 结束仍在运行的「调试版」白泽（target\debug 下的 baize.exe）。
// 运行中的 exe 会被 Windows 锁定，链接器无法替换 →
// 「failed to remove file ... baize.exe · 拒绝访问 (os error 5)」。
// 只按进程路径过滤 target\debug 目录，不影响用户安装的正式版。
// 由 tauri.conf.json 的 beforeDevCommand / beforeBuildCommand 在每次构建前确定性调用。
import { execSync, spawn } from "node:child_process";
import { statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

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
// 派生一个 10 分钟的后台守护，每 0.5s 查杀一次；
// 一旦 exe 被新版本替换（链接完成，mtime 变化）立即退出，
// 不影响 tauri dev 在构建完成后拉起新实例。detached 启动，脚本退出后守护独立存活。
const exe = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "src-tauri",
  "target",
  "debug",
  "baize.exe"
);

let born = 0;
try {
  born = Math.floor(statSync(exe).mtimeMs / 1000);
} catch {
  // exe 尚不存在（首次构建）时 born=0，守护只负责查杀
}

const watcher = `
$exe='${exe.replace(/'/g, "''")}'; $born=${born}; $end=(Get-Date).AddMinutes(10)
while((Get-Date) -lt $end) {
  if(Test-Path -LiteralPath $exe) {
    $now=[long](Get-Item -LiteralPath $exe).LastWriteTimeUtc.Subtract([datetime]'1970-01-01').TotalSeconds
    if($now -ne $born) { exit }
  }
  ${PS_KILL}
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
