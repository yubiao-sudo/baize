// ==========================================================================
// 设置细胞 · SettingsPrism
//   listens:  prism.changed
//   emits:    （通过直接调用 applyPrism 触发 prism.changed 广播）
// ==========================================================================
import { useEffect, useState } from "react";
import { PrismCell } from "../PrismCell";
import { useCell } from "../cell.hooks";
import {
  applyPrism,
  getActivePrism,
  listPrismThemes,
  getPrismById,
} from "../../prism/prism.engine";
import { prismBus } from "../../bus/prism.bus";

export function SettingsPrism() {
  const themes = listPrismThemes();
  const [activeId, setActiveId] = useState<string>(getActivePrism().id);

  useCell({ name: "设置·棱镜面板", category: "settings" }, {
    "prism.changed": (env) => setActiveId((env.payload as { themeId: string }).themeId),
  });

  function handleSwitch(id: string) {
    applyPrism(id);
  }

  return (
    <PrismCell
      title="设置·棱镜面板"
      subtitle="Prism Settings · Theme / Bus Audit"
    >
      <div style={{ fontSize: 12, color: "var(--prism-ink-2)", marginBottom: 10 }}>
        折射配色（切换即生效，<code>prism.changed</code> 信号广播给所有 Cell）
      </div>
      <div className="theme-grid">
        {themes.map((tm) => {
          const full = getPrismById(tm.id);
          const style = full
            ? ({
                ["--t1" as never]: full.tokens.prism_face_r,
                ["--t2" as never]: full.tokens.prism_face_g,
                ["--t3" as never]: full.tokens.prism_face_b,
              } as React.CSSProperties)
            : undefined;
          return (
            <div
              key={tm.id}
              className={`theme-card ${activeId === tm.id ? "active" : ""}`}
              onClick={() => handleSwitch(tm.id)}
            >
              <div className="theme-swatch" style={style} />
              <h5>{tm.name}</h5>
              <p>{tm.description}</p>
            </div>
          );
        })}
      </div>

      <div style={{ marginTop: 18, paddingTop: 14, borderTop: "1px dashed var(--prism-cell-edge)" }}>
        <div style={{ fontSize: 12, color: "var(--prism-ink-2)", marginBottom: 10 }}>
          信号总线审计（最近 24 条 · kind / tier / source）
        </div>
        <AuditTail />
      </div>
    </PrismCell>
  );
}

function AuditTail() {
  const [rows, setRows] = useState<Array<{ kind: string; tier: string; source: string }>>([]);
  useEffect(() => {
    const tick = () => {
      const snap = prismBus.inspect("", 24).map((e) => ({
        kind: e.kind, tier: e.tier, source: e.source,
      }));
      setRows(snap);
    };
    tick();
    const t = setInterval(tick, 350);
    return () => clearInterval(t);
  }, []);
  const tierColor = (t: string) =>
    t === "atomic"  ? "var(--prism-face-b)" :
    t === "pulse"   ? "var(--prism-face-g)" :
    t === "wave"    ? "var(--prism-axis)"   :
    t === "prism"   ? "var(--prism-face-r)" :
                      "var(--prism-danger)";
  return (
    <div
      className="scroll-y"
      style={{
        maxHeight: 170,
        background: "color-mix(in srgb, var(--prism-bg-void) 70%, transparent)",
        border: "1px solid color-mix(in srgb, var(--prism-cell-edge) 60%, transparent)",
        borderRadius: 10, padding: "6px 10px",
        fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace", fontSize: 11.5,
      }}
    >
      {rows.length === 0 ? (
        <div style={{ color: "var(--prism-ink-3)", padding: 8 }}>等待信号…</div>
      ) : (
        rows.map((r, i) => (
          <div
            key={i}
            style={{
              display: "grid", gridTemplateColumns: "84px 1fr auto", gap: 8,
              padding: "2px 0",
              borderBottom: "1px dashed color-mix(in srgb, var(--prism-cell-edge) 30%, transparent)",
            }}
          >
            <span style={{ color: tierColor(r.tier), letterSpacing: "0.08em", textTransform: "uppercase" }}>{r.tier}</span>
            <span style={{ color: "var(--prism-ink-1)" }}>{r.kind}</span>
            <span style={{ color: "var(--prism-ink-3)" }}>{r.source}</span>
          </div>
        ))
      )}
    </div>
  );
}