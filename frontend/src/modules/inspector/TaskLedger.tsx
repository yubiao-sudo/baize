import { useChat } from "../../kernel/store/chat";

export function TaskLedger() {
  const todos = useChat((s) => s.todos);
  const done = todos.filter((t) => t.status === "completed").length;
  const total = todos.length;
  const pct = total === 0 ? 0 : Math.round((done / total) * 100);

  return (
    <>
      <div className="pad-header">
        <span className="pad-sigil" style={{ background: "var(--aurora-g)" }} />
        <span className="pad-title">任务台账</span>
        <span className="pad-sub">TODO · LEDGER</span>
      </div>
      <div className="pad-body">
        {total === 0 ? (
          <div className="thought-empty">◌ 暂无待办 · EMPTY</div>
        ) : (
          <>
            <div className="td-ribbon">
              <span className="td-count">{done}/{total}  ·  {pct}%</span>
              <div className="td-line"><span style={{ width: pct + "%" }} /></div>
            </div>
            {todos.map((t) => (
              <div className={`taskline ${t.status}`} key={t.id} title={t.title}>
                <span className="tl-marker" />
                <span className="tl-title">{t.title}</span>
              </div>
            ))}
          </>
        )}
      </div>
    </>
  );
}