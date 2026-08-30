import { useEffect, useState } from "react";
import { useDock } from "../../kernel/store/dock";

const LINES = [
  ["I", "水晶工坊启动序列 · 晶核预热…", 140],
  ["I", "链路自检 · 云端优先 → 本地回退 ……  OK", 220],
  ["I", "契约校验 · 40+ Tauri 命令签名 ……  通过", 180],
  ["I", "审批联锁 · 五级升级链就绪 ……  OK", 160],
  ["I", "记忆星图 · 紫晶节点扫描 ……  28 颗 · 召回 4 颗", 240],
  ["I", "灵感池 · 极光波动稳定 ……  OK", 120],
  ["S", "水晶工坊就绪 · 欢迎回来。", 0],
] as const;

export function BootSequence() {
  const boot = useDock((s) => s.boot);
  const completeBoot = useDock((s) => s.completeBoot);
  const [idx, setIdx] = useState(0);
  const [pct, setPct] = useState(0);

  useEffect(() => {
    if (idx >= LINES.length) {
      // 停 400ms 再完成（观感）
      const t = setTimeout(() => completeBoot(), 400);
      return () => clearTimeout(t);
    }
    const delay = LINES[idx][2];
    setPct(Math.round(((idx + 1) / LINES.length) * 100));
    const t = setTimeout(() => setIdx((i) => i + 1), delay);
    return () => clearTimeout(t);
  }, [idx, completeBoot]);

  const skip = () => {
    setPct(100);
    setIdx(LINES.length);
  };

  if (boot) return null;
  return (
    <div className="bootstage" onClick={skip}>
      <div className="boot-stage-inner">
        <div className="boot-logo-row"><div className="minigem" /></div>
        {LINES.slice(0, idx).map((l, i) => (
          <div className="boot-line" key={i}>
            {l[0] === "S" ? (
              <em>{l[1]}</em>
            ) : (
              <>
                <i>[{String(i + 1).padStart(2, "0")}]</i> {l[1]}
              </>
            )}
          </div>
        ))}
        <div className="boot-progress"><span style={{ width: pct + "%" }} /></div>
        <div className="boot-hint">◈ 水晶工坊 · CRYSTAL · WORKSHOP · 点击任意位置跳过</div>
      </div>
    </div>
  );
}