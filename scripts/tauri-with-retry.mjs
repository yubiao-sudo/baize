// tauri 命令包装：解决「应用运行中重新编译 → 链接器无法替换被锁定的 baize.exe →
// 拒绝访问 (os error 5)」的问题。
// 策略：每次调用前先清理残留的调试版实例（确定性，覆盖 99% 场景）；
// 若构建期间实例又被拉起导致链接失败（os error 5），自动清理后重试（最多 2 次）。
import { spawn, spawnSync, execSync } from "node:child_process";

const args = process.argv.slice(2);
const MAX_RETRY = 2;

function killStale() {
  try {
    execSync("node scripts/kill-debug-baize.mjs", { stdio: "inherit" });
  } catch {
    /* 清理失败不阻断构建（build.rs 里的兜底仍会尝试） */
  }
}

let attempt = 0;
while (true) {
  attempt++;
  killStale();

  // stderr 过管道：边透出实时输出边扫描失败特征；stdout 保持直通
  const child = spawn(`tauri ${args.join(" ")}`, {
    shell: true,
    stdio: ["inherit", "inherit", "pipe"],
  });
  let errText = "";
  child.stderr.on("data", (chunk) => {
    const text = chunk.toString();
    errText += text;
    if (errText.length > 64 * 1024) errText = errText.slice(-32 * 1024);
    process.stderr.write(text);
  });

  const code = await new Promise((resolve) => child.on("close", resolve));
  if (code === 0) process.exit(0);

  const lockFailure = /os error 5|拒绝访问|failed to remove file/.test(errText);
  if (lockFailure && attempt <= MAX_RETRY) {
    console.log(
      `\n[tauri] 检测到调试版实例占用了输出文件（os error 5），已自动清理并重试（第 ${attempt}/${MAX_RETRY} 次）…\n`
    );
    continue;
  }
  process.exit(code ?? 1);
}
