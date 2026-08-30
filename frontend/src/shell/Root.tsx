import { useCallback } from "react";
import { useRuntimeEvents } from "./useRuntimeEvents";
import { TopDock } from "./TopDock";
import { WorkbenchGrid } from "./WorkbenchGrid";
import { Composer } from "./Composer";

import { SessionTree } from "../modules/session/SessionTree";
import { AttachmentShelf } from "../modules/session/AttachmentShelf";
import { ArchiveDrawer } from "../modules/session/ArchiveDrawer";

import { ThreadStream } from "../modules/canvas/ThreadStream";
import { ResponseCrystal } from "../modules/canvas/ResponseCrystal";
import { TraceDrawer } from "../modules/canvas/TraceDrawer";
import { WelcomeCore } from "../modules/canvas/WelcomeCore";

import { ThoughtStream } from "../modules/inspector/ThoughtStream";
import { TaskLedger } from "../modules/inspector/TaskLedger";
import { MemoryGraph } from "../modules/inspector/MemoryGraph";

import { ApprovalRail } from "../modules/approval/ApprovalRail";
import { SettingsConsole } from "../modules/settings/SettingsConsole";
import { NoticeStack } from "../modules/notify/NoticeStack";
import { BootSequence } from "../modules/boot/BootSequence";

import { useChat } from "../kernel/store/chat";
import { useDock } from "../kernel/store/dock";

/**
 * 水晶工坊 · Root Shell
 *  [TopDock]
 *  [LeftAuxDock (SessionTree / AttachmentShelf)] + [WorkbenchGrid] + [RightAuxDock (Thoughts/Tasks/Memory)]
 *  [Composer]
 *  浮层 (Boot · Approval · Settings · Notify · Archive)
 */
export default function Root() {
  useRuntimeEvents();
  const send = useChat((s) => s.send);
  const boot = useDock((s) => s.boot);

  const jumpAssistant = useCallback((_idx: number) => {
    // 水晶应答台右侧自动滚动至末尾
    const el = document.querySelector<HTMLDivElement>(".pad-response .pad-body > div");
    if (el) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, []);

  const welcome = <WelcomeCore send={(t) => send(t, [])} />;

  return (
    <div className="stage">
      <div className="stars" />
      <div className="bloom-1" />
      <div className="bloom-2" />
      <div className="bloom-3" />
      <div className="aurora" />

      <TopDock />

      <div className="dockrow">
        <aside className="auxdock left">
          <div className="glass"><SessionTree /></div>
          <div className="glass" style={{ flex: 1, minHeight: 0 }}><AttachmentShelf /></div>
        </aside>

        <WorkbenchGrid
          thread={<ThreadStream onJumpAssistant={jumpAssistant} empty={welcome} />}
          response={<ResponseCrystal empty={welcome} />}
          trace={<TraceDrawer />}
        />

        <aside className="auxdock right">
          <div className="glass" style={{ flex: 1, minHeight: 0 }}><ThoughtStream /></div>
          <div className="glass" style={{ flex: "0 0 auto" }}><TaskLedger /></div>
          <div className="glass" style={{ flex: "0 0 240px" }}><MemoryGraph /></div>
        </aside>
      </div>

      <Composer />

      {!boot && <BootSequence />}
      <ApprovalRail />
      <ArchiveDrawer />
      <SettingsConsole />
      <NoticeStack />
    </div>
  );
}