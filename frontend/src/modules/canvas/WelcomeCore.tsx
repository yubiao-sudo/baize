import { useChat } from "../../kernel/store/chat";

const ENTRIES = [
  { g: "❖", title: "拆解一段代码并列出改进建议", hint: "让白泽审阅并优化一段函数或模块" },
  { g: "✱", title: "生成一份测试用例（文本 / 文件路径均可）", hint: "支持 txt / md / csv / docx / pdf 抽取" },
  { g: "⟡", title: "在屏幕上自动化执行一串 GUI 操作", hint: "三级接地能力 · OCR 中文优先 · 紧急 Ctrl+Shift+F12" },
  { g: "◉", title: "开启协作执行，分配成员共享工具完成任务", hint: "规划 → 分工执行 → 交付总结 三阶段流程" },
  { g: "❂", title: "从记忆中召回相关经验再回答", hint: "白泽会按主题自动查询项目上下文" },
];

export function WelcomeCore({ send }: { send: (t: string) => void }) {
  return (
    <div className="welcome">
      <div className="welcome-inner">
        <div className="gemstone" />
        <div className="wel-title">白泽水晶工坊</div>
        <div className="wel-sub">BZ · CRYSTAL · WORKSHOP</div>
        <p className="wel-desc">
          紫晶为核、极光为幕。以纯粹的创作姿态，
          让灵感化作可被雕琢的切面，在这里构思、对话、生成、交付。
        </p>
        <div className="craft-entries">
          {ENTRIES.map((e) => (
            <button key={e.title} className="craft-card" onClick={() => send(e.title)}>
              <span className="craft-glyph">{e.g}</span>
              <div style={{ flex: 1 }}>
                <div className="craft-label">{e.title}</div>
                <div className="craft-hint">{e.hint}</div>
              </div>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}