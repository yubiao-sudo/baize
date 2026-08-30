import { useEffect, useRef } from "react";
import { useChat } from "../../core/store/chat";
import MsgCapsule from "./MsgCapsule";
import HomeCore from "./HomeCore";

/** 中央对话流：消息胶囊 + 流式输出 + 自动贴底 */
export default function StreamColumn() {
  const history = useChat((s) => s.history);
  const streaming = useChat((s) => s.streaming);
  const busy = useChat((s) => s.busy);
  const comparing = useChat((s) => s.comparing);
  const ref = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);

  useEffect(() => {
    const el = ref.current;
    if (el && stickRef.current) el.scrollTop = el.scrollHeight;
  }, [history.length, streaming, busy, comparing]);

  const onScroll = () => {
    const el = ref.current;
    if (!el) return;
    stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
  };

  const empty = history.length === 0 && !busy && !comparing && !streaming;

  return (
    <div className="stream-col" onScroll={onScroll}>
      {empty ? (
        <HomeCore />
      ) : (
        <div className="stream-inner">
          {history.map((m, i) => (
            <MsgCapsule key={i} msg={m} />
          ))}
          {streaming ? (
            <MsgCapsule msg={{ role: "assistant", content: streaming }} live />
          ) : (
            (busy || comparing) && (
              <div className="cap assist pending">
                <div className="cap-h">
                  <span className="cap-tag">白泽</span>
                  <span className="cap-state">{comparing ? "多模型并行推演" : "运算中"}</span>
                </div>
                <div className="cap-b thinking">
                  <i />
                  <i />
                  <i />
                </div>
              </div>
            )
          )}
        </div>
      )}
    </div>
  );
}