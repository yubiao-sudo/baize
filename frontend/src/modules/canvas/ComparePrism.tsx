import { useEffect, useState } from "react";
import { useDock } from "../../kernel/store/dock";
import { useChat } from "../../kernel/store/chat";
import { subscribeCompareDone, subscribeCompareError, subscribeCompareToken } from "../../kernel/api";
import { renderMd } from "../../lib/md";

interface BranchState {
  name: string;
  model_id: string;
  content: string;
  error: string | null;
  done: boolean;
}

/**
 * 多模型对比棱镜：横向并排卡片
 * · 流式 token 追加 · 完成态 · 错误态
 */
export function ComparePrism() {
  const models = useDock((s) => s.activeModels.filter((m) => m.enabled));
  const comparing = useChat((s) => s.comparing);
  const [branches, setBranches] = useState<BranchState[]>([]);

  useEffect(() => {
    if (comparing) {
      setBranches(models.map((m) => ({ name: m.name, model_id: m.model_id, content: "", error: null, done: false })));
    }
  }, [comparing, models]);

  useEffect(() => {
    const uns: (void | (() => void))[] = [];
    uns.push(
      subscribeCompareToken<{ idx: number; token: string }>((e) => {
        setBranches((bs) => bs.map((b, i) => (i === e.payload.idx ? { ...b, content: b.content + e.payload.token } : b)));
      }),
    );
    uns.push(subscribeCompareDone(() => setBranches((bs) => bs.map((b) => ({ ...b, done: true })))));
    uns.push(
      subscribeCompareError<{ idx?: number; error: string }>((e) => {
        const i = e.payload.idx ?? 0;
        setBranches((bs) => bs.map((b, k) => (k === i ? { ...b, error: e.payload.error, done: true } : b)));
      }),
    );
    return () => { uns.forEach((f) => typeof f === "function" && f()); };
  }, []);

  if (!comparing && branches.length === 0) return null;
  return (
    <div style={{ marginTop: 12 }}>
      <div style={{ fontFamily: "var(--mono)", fontSize: 11, color: "var(--faint)", letterSpacing: 1.5, marginBottom: 6, paddingLeft: 4 }}>
        ◈ 棱镜对比 · {branches.filter((b) => b.done).length}/{branches.length}
      </div>
      <div className="branchgrid">
        {branches.map((b, i) => (
          <div key={i} className={`branchcard ${b.error ? "err" : ""}`}>
            <div className="branch-h">
              <span className="pad-sigil" style={{ width: 4, height: 4 }} />
              <strong>{b.name}</strong>
              <code>{b.model_id}</code>
            </div>
            {b.error ? (
              <div className="branch-err">⚠ {b.error}</div>
            ) : (
              <div className="branch-body">
                <div className="md" dangerouslySetInnerHTML={{ __html: renderMd(b.content || "…") }} />
                {!b.done && <div className="thinkdots" style={{ marginTop: 6 }}><i /><i /><i /></div>}
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

