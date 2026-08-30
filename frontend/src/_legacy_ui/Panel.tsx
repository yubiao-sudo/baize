import type { ReactNode } from "react";

interface Props {
  title: string;
  right?: ReactNode;
  children: ReactNode;
  className?: string;
}

/** HUD 面板原语：角括号 + 标题条 + 内容区 */
export default function Panel({ title, right, children, className }: Props) {
  return (
    <section className={`panel ${className ?? ""}`}>
      <header className="panel-h">
        <span className="panel-t">{title}</span>
        {right && <span className="panel-r">{right}</span>}
      </header>
      <div className="panel-b">{children}</div>
    </section>
  );
}