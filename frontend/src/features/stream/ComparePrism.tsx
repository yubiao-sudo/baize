import { memo, useMemo } from "react";
import { renderMd } from "../../lib/md";
import type { ModelAnswer } from "../../core/types";

function PrismCol({ b }: { b: ModelAnswer }) {
  const html = useMemo(() => renderMd(b.content ?? ""), [b.content]);
  return (
    <div className={`prism-col ${b.error ? "err" : ""}`}>
      <div className="prism-h">
        <i className={`tier ${b.tier}`}>{b.tier === "cloud" ? "云端" : "本地"}</i>
        <span className="prism-name">{b.name}</span>
        <code className="prism-model">{b.model}</code>
      </div>
      {b.error ? (
        <div className="prism-error">⚠ {b.error}</div>
      ) : (
        <div className="prism-b md" dangerouslySetInnerHTML={{ __html: html }} />
      )}
    </div>
  );
}

/** 分支棱镜：同一问题下多模型并行应答的并排对比 */
function ComparePrism({ branches }: { branches: ModelAnswer[] }) {
  return (
    <div className="prism">
      {branches.map((b) => (
        <PrismCol key={b.name + b.model} b={b} />
      ))}
    </div>
  );
}

export default memo(ComparePrism);