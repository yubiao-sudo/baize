import { useHud } from "../../core/store/hud";
import Panel from "../../ui/Panel";
import ThoughtFeed from "./ThoughtFeed";
import TaskLedger from "./TaskLedger";
import MemoryNebula from "./MemoryNebula";

/** 右侧遥测栈：思维流 / 任务清单 / 记忆星云 */
export default function TelemetryStack() {
  const telemetry = useHud((s) => s.telemetry);
  const nebula = useHud((s) => s.nebula);
  const toggleNebula = useHud((s) => s.toggleNebula);
  if (!telemetry) return null;

  return (
    <aside className="telemetry">
      <Panel
        title="思维遥测"
        className="p-grow"
        right={
          <button
            className={`mini-btn ${nebula ? "on" : ""}`}
            onClick={toggleNebula}
            title="记忆星云"
          >
            ✷
          </button>
        }
      >
        <ThoughtFeed />
      </Panel>
      <Panel title="任务清单">
        <TaskLedger />
      </Panel>
      {nebula && (
        <Panel title="记忆星云" className="p-nebula">
          <MemoryNebula />
        </Panel>
      )}
    </aside>
  );
}