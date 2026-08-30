import { useHud } from "../core/store/hud";
import { useBackendEvents } from "./useBackendEvents";
import Boot from "./Boot";
import NoticeBoard from "./NoticeBoard";
import StatusRail from "./layout/StatusRail";
import Spine from "./layout/Spine";
import CommandDeck from "./layout/CommandDeck";
import StreamColumn from "../features/stream/StreamColumn";
import TelemetryStack from "../features/telemetry/TelemetryStack";
import Interlock from "../features/approvals/Interlock";
import ArchiveDrawer from "../features/archive/ArchiveDrawer";
import CoreConsole from "../features/settings/CoreConsole";

/** 全息作战舱：状态带 / 功能脊 / 对话流 / 遥测栈 / 指令台 */
export default function App() {
  useBackendEvents();
  const booted = useHud((s) => s.booted);
  const archive = useHud((s) => s.archive);
  const settings = useHud((s) => s.settings);

  return (
    <div className="cockpit">
      <div className="bg-grid" />
      <div className="bg-glow" />
      <div className="bg-scan" />
      {!booted && <Boot />}
      <StatusRail />
      <div className="deck-row">
        <Spine />
        <StreamColumn />
        <TelemetryStack />
      </div>
      <CommandDeck />
      <Interlock />
      {archive && <ArchiveDrawer />}
      {settings && <CoreConsole />}
      <NoticeBoard />
    </div>
  );
}