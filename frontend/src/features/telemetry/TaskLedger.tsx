import { useChat } from "../../core/store/chat";

const ICON = { pending: "○", in_progress: "◐", completed: "●" } as const;

/** 任务台账：后端推送的任务拆解与步骤状态 */
export default function TaskLedger() {
  const todos = useChat((s) => s.todos);
  if (todos.length === 0) {
    return <div className="tl-empty">暂无任务清单</div>;
  }
  const done = todos.filter((t) => t.status === "completed").length;
  return (
    <div className="tl-list">
      <div className="tl-progress">
        <span className="tl-count">{done}/{todos.length}</span>
        <div className="tl-bar">
          <span style={{ width: `${(done / todos.length) * 100}%` }} />
        </div>
      </div>
      {todos.map((t) => (
        <div key={t.id} className={`tl-row ${t.status}`}>
          <i className="tl-icon">{ICON[t.status]}</i>
          <span className="tl-title">{t.title}</span>
        </div>
      ))}
    </div>
  );
}