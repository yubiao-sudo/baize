import { useChat } from "../../core/store/chat";
import { useHud } from "../../core/store/hud";

/** 左侧功能脊：菱形节点 + 悬浮标签 */
export default function Spine() {
  const newConversation = useChat((s) => s.newConversation);
  const conversations = useChat((s) => s.conversations);
  const telemetry = useHud((s) => s.telemetry);
  const toggleTelemetry = useHud((s) => s.toggleTelemetry);
  const toggleArchive = useHud((s) => s.toggleArchive);
  const toggleSettings = useHud((s) => s.toggleSettings);

  return (
    <nav className="spine">
      <div className="node-wrap">
        <button className={`node ${telemetry ? "on" : ""}`} onClick={toggleTelemetry} title="遥测栈">
          <i>◈</i>
        </button>
        <span className="node-label">遥测栈</span>
      </div>
      <div className="node-wrap">
        <button className="node" onClick={() => void newConversation()} title="新会话">
          <i>✚</i>
        </button>
        <span className="node-label">新会话</span>
      </div>
      <div className="node-wrap">
        <button className="node" onClick={toggleArchive} title="会话档案">
          <i>⧉</i>
        </button>
        {conversations.length > 0 && <b className="node-badge">{conversations.length}</b>}
        <span className="node-label">档案 {conversations.length}</span>
      </div>
      <div className="spine-gap" />
      <div className="node-wrap">
        <button className="node" onClick={toggleSettings} title="核心控制台">
          <i>⚙</i>
        </button>
        <span className="node-label">控制台</span>
      </div>
      <div className="spine-foot">BZ-IND // TALOS-Ⅱ</div>
    </nav>
  );
}