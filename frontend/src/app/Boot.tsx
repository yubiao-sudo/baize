import { useEffect, useState } from "react";
import { useHud } from "../core/store/hud";

const LINES = [
  "白泽工业核心 BZ-CORE // TALOS-Ⅱ",
  "终端自检 ▸ 文件读写 …… OK",
  "终端自检 ▸ 屏幕感知 …… OK",
  "终端自检 ▸ 记忆星图 …… OK",
  "链路自检 ▸ 云端优先 → 本地回退",
  "权限联锁 ▸ 五级升级链就绪",
  "协议就绪 · 欢迎回来，管理员",
];

/** 启动自检序列：逐行点亮诊断，随后交出控制权 */
export default function Boot() {
  const setBooted = useHud((s) => s.setBooted);
  const [n, setN] = useState(0);

  useEffect(() => {
    if (n >= LINES.length) {
      const t = setTimeout(() => setBooted(), 650);
      return () => clearTimeout(t);
    }
    const t = setTimeout(() => setN((v) => v + 1), 240);
    return () => clearTimeout(t);
  }, [n, setBooted]);

  return (
    <div className="boot" onClick={() => setBooted()}>
      <div className="boot-box">
        <div className="boot-sigil">◈</div>
        {LINES.slice(0, n).map((l, i) => (
          <div key={i} className="boot-line">{l}</div>
        ))}
        <div className="boot-bar">
          <span style={{ width: `${(n / LINES.length) * 100}%` }} />
        </div>
        <div className="boot-hint">点击任意处跳过</div>
      </div>
    </div>
  );
}