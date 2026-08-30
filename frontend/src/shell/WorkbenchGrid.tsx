import { ReactNode } from "react";

interface Props {
  thread: ReactNode;
  response: ReactNode;
  trace: ReactNode;
}

/**
 * 水晶工坊 · Workbench Grid
 * 1.25fr thread  | 1fr response
 * -------- trace (横跨双列) --------
 */
export function WorkbenchGrid({ thread, response, trace }: Props) {
  return (
    <main className="workbench">
      <section className="glass pad-thread">
        <div className="pad-header">
          <span className="pad-sigil" />
          <span className="pad-title">对话时间线</span>
          <span className="pad-sub">THREAD · TIMELINE</span>
        </div>
        <div className="pad-body">{thread}</div>
      </section>

      <section className="glass pad-response">
        <div className="pad-header">
          <span className="pad-sigil" style={{ background: "var(--orchid)" }} />
          <span className="pad-title">水晶应答台</span>
          <span className="pad-sub">RESPONSE · CRYSTAL</span>
        </div>
        <div className="pad-body">{response}</div>
      </section>

      <section className="glass pad-trace">
        <div className="pad-header">
          <span className="pad-sigil" style={{ background: "var(--aurora-g)" }} />
          <span className="pad-title">执行流抽屉</span>
          <span className="pad-sub">TRACE · EXEC</span>
          <div className="pad-tools" />
        </div>
        <div className="pad-body">{trace}</div>
      </section>
    </main>
  );
}