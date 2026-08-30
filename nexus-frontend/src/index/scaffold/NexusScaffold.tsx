// ==========================================================================
// 棱镜舱脚手架 + 顶栏
// Nexus Scaffold（五段栅格） · 顶栏 TopRail（品牌/IM 状态/时钟/主题切换）
// ==========================================================================
import { useEffect, useState, type ReactNode } from "react";
import { ConversationThread } from "../../cells/conversation/ConversationThread";
import { ToolStream } from "../../cells/toolstream/ToolStream";
import { ApprovalGuard } from "../../cells/approval/ApprovalGuard";
import { MemoryPrism, SessionTree } from "../../cells/memory/SessionTree_MemoryPrism";
import { SettingsPrism } from "../../cells/settings/SettingsPrism";
import { applyPrism, getActivePrism, listPrismThemes } from "../../prism/prism.engine";

// ---- 顶栏 ----
export function NexusTopRail() {
  const [now, setNow] = useState<Date>(new Date());
  const [themeOpen, setThemeOpen] = useState(false);
  const themes = listPrismThemes();
  const [activeThemeId, setActiveThemeId] = useState(getActivePrism().id);

  useEffect(() => {
    const t = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(t);
  }, []);

  return (
    <section className="prism-cell nexus-topbar" style={{ borderRadius: 14 }}>
      <div className="nexus-brand">
        <div className="nexus-brand-logo" />
        <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
          <h1>白泽 · 棱镜舱</h1>
          <small>BAIZE · PRISM NEXUS · v3.0</small>
        </div>
      </div>

      <div className="nexus-top-actions">
        <div className="nexus-chips">
          <span className="nexus-chip ok" title="Tauri 桌面连接状态（预览模式显示为 Browser）">
            ● {typeof window !== "undefined" && "__TAURI_INTERNALS__" in window ? "TAURI" : "BROWSER"}
          </span>
          <span className="nexus-chip ok" title="微信 IM 连接状态（预览下为 mock）">
            微信 OK
          </span>
          <span className="nexus-chip warn" title="飞书 IM 连接状态（预览下为 mock）">
            飞书 MID
          </span>
          <span className="nexus-chip">
            📡 {now.toLocaleTimeString("zh-CN", { hour12: false })}
          </span>
        </div>

        <div className="model-pick">
          <button
            className="prism-btn ghost"
            onClick={() => setThemeOpen((v) => !v)}
            title="切换折射主题"
          >
            <span style={{
              width: 10, height: 10, borderRadius: 3,
              background: `linear-gradient(135deg, var(--prism-face-r), var(--prism-face-g) 50%, var(--prism-face-b))`,
              boxShadow: "0 0 6px color-mix(in srgb, var(--prism-axis) 60%, transparent)",
            }} />
            折射 · {themes.find((t) => t.id === activeThemeId)?.name ?? activeThemeId}
            <span style={{ fontSize: 10, opacity: 0.7 }}>▼</span>
          </button>
          {themeOpen ? (
            <div className="model-menu" onMouseLeave={() => setThemeOpen(false)}>
              {themes.map((t) => (
                <div
                  key={t.id}
                  className="mm-item"
                  onClick={() => {
                    applyPrism(t.id);
                    setActiveThemeId(t.id);
                    setThemeOpen(false);
                  }}
                >
                  <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                    <span
                      style={{
                        width: 12, height: 12, borderRadius: 4,
                        background: `linear-gradient(135deg, var(--t1), var(--t2), var(--t3))`,
                        border: "1px solid color-mix(in srgb, var(--prism-cell-edge) 60%, transparent)",
                      }}
                    />
                    <span>{t.name}</span>
                  </div>
                  <span className="mm-tag">{t.id}</span>
                </div>
              ))}
            </div>
          ) : null}
        </div>
      </div>
    </section>
  );
}

// ---- 脚手架（五段栅格） ----
export function NexusScaffold() {
  const [tab, setTab] = useState<"tools" | "settings">("tools");

  return (
    <div className="nexus-scaffold">
      <div className="zone-top">
        <NexusTopRail />
      </div>

      <aside className="zone-left">
        <SessionTree />
        <div className="stretch">
          <MemoryPrism />
        </div>
      </aside>

      <main className="zone-center">
        <ConversationThread />
      </main>

      <aside className="zone-right">
        <ApprovalGuard />
        <div style={{ display: "flex", gap: 6 }}>
          <TabButton label="工具流"  active={tab === "tools"}    onClick={() => setTab("tools")} />
          <TabButton label="设置"    active={tab === "settings"} onClick={() => setTab("settings")} />
        </div>
        <div className="stretch">
          {tab === "tools" ? <ToolStream /> : <SettingsPrism />}
        </div>
      </aside>
    </div>
  );
}

function TabButton({ label, active, onClick }: { label: string; active: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={`prism-btn ${active ? "" : "ghost"}`}
      style={{ flex: 1, padding: "6px 10px", fontSize: 12, letterSpacing: "0.08em" }}
    >
      {label}
    </button>
  );
}

// ---- 根：背景三层 sheen + 噪点 + 脚手架 ----
export function NexusRoot({ children }: { children?: ReactNode }) {
  return (
    <div className="nexus-root">
      <div className="nexus-grain" />
      {children ?? <NexusScaffold />}
    </div>
  );
}