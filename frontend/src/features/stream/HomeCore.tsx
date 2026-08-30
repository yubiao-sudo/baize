import { useHud } from "../../core/store/hud";

const SUGGESTIONS = [
  "读取屏幕，总结我当前桌面上正在发生什么",
  "搜索今天的科技新闻热点，并给出简报",
  "帮我起草一份本周工作周报",
  "用多模型分支对比：如何设计本地优先的桌面 Agent",
];

/** 空舱欢迎核：徽记 + 指令种子 */
export default function HomeCore() {
  const setDraft = useHud((s) => s.setDraft);
  return (
    <div className="homecore">
      <div className="hc-sigil">◈</div>
      <h1 className="hc-title">白泽工业终端</h1>
      <p className="hc-sub">TALOS-Ⅱ 先遣协议 · 本地优先 · 写操作需审批</p>
      <div className="hc-chips">
        {SUGGESTIONS.map((s) => (
          <button key={s} className="hc-chip" onClick={() => setDraft(s)}>
            {s}
          </button>
        ))}
      </div>
    </div>
  );
}