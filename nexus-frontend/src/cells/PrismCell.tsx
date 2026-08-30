// 通用 Cell 外壳（把 Cell 元信息 + 标题 + 可选工具按钮统一渲染）
import type { ReactNode } from "react";

export interface PrismCellProps {
  title: string;
  subtitle?: string;
  className?: string;
  /** 标题右侧的工具按钮区域 */
  tools?: ReactNode;
  children: ReactNode;
  bodyClassName?: string;
}

export function PrismCell({ title, subtitle, className = "", tools, children, bodyClassName = "" }: PrismCellProps) {
  return (
    <section className={`prism-cell ${className}`}>
      <header className="prism-cell-title">
        <span className="prism-dot" />
        <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
          <span>{title}</span>
          {subtitle ? (
            <span
              style={{
                fontSize: 10.5,
                fontWeight: 500,
                color: "var(--prism-ink-3)",
                letterSpacing: "0.08em",
                textTransform: "none",
              }}
            >
              {subtitle}
            </span>
          ) : null}
        </div>
        <div style={{ marginLeft: "auto", display: "flex", gap: 6 }}>{tools}</div>
      </header>
      <div className={`prism-cell-body ${bodyClassName}`}>{children}</div>
    </section>
  );
}