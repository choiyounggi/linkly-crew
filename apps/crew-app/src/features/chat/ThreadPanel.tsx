// Right-side thread panel (t7 plan D1 exception, Task 03). App.tsx (t6-owned)
// reserves a 360px grid column via `<aside id="channel-side-panel">` but
// exposes no ref/children/portal-target prop — ChatPane only ever receives
// {runId}. Per the coordinator's decision (decisions.md, App.tsx aside
// id=channel-side-panel), this portals into that element by id; when the
// target isn't mounted (e.g. ThreadPanel rendered standalone in a test),
// it falls back to rendering inline in ChatPane's own tree instead of
// crashing or silently disappearing.

import { createPortal } from "react-dom";

import { useRunStore } from "../../lib/store";
import { parseHumanGateBody } from "./body";
import { gateResolutions } from "./derive";
import { EMPTY_MESSAGES, EMPTY_READ_RECEIPTS, EMPTY_READERS } from "./empty";
import MessageRow from "./MessageRow";

const SIDE_PANEL_TARGET_ID = "channel-side-panel";

interface ThreadPanelProps {
  runId: string;
  threadId: string;
  onClose: () => void;
}

function ThreadPanelContent({ runId, threadId, onClose, mounted }: ThreadPanelProps & { mounted: boolean }) {
  const messages = useRunStore((s) => s.channels[runId]?.messages ?? EMPTY_MESSAGES);
  const readReceipts = useRunStore((s) => s.channels[runId]?.readReceipts ?? EMPTY_READ_RECEIPTS);
  const items = messages.filter(({ envelope }) => envelope.thread === threadId);
  const resolutions = gateResolutions(items);

  return (
    <div className="thread-panel" aria-label="스레드 패널" data-portal-mounted={mounted || undefined}>
      <div className="thread-panel__header">
        <h2 className="thread-panel__title">{`스레드: ${threadId}`}</h2>
        <button type="button" className="thread-panel__close" onClick={onClose} aria-label="스레드 패널 닫기">
          ×
        </button>
      </div>
      {items.length === 0 ? (
        <p className="thread-panel__empty">메시지가 없습니다</p>
      ) : (
        <ul className="thread-panel__list">
          {items.map(({ envelope }) => {
            const gateTaskId = envelope.kind === "human.gate" ? parseHumanGateBody(envelope.body)?.task_id : undefined;
            return (
              <MessageRow
                key={envelope.id}
                runId={runId}
                envelope={envelope}
                replyCount={0}
                readers={readReceipts[envelope.id] ?? EMPTY_READERS}
                gateResolution={gateTaskId ? (resolutions.get(gateTaskId) ?? null) : null}
              />
            );
          })}
        </ul>
      )}
    </div>
  );
}

export default function ThreadPanel(props: ThreadPanelProps) {
  const target = typeof document !== "undefined" ? document.getElementById(SIDE_PANEL_TARGET_ID) : null;
  const content = <ThreadPanelContent {...props} mounted={Boolean(target)} />;
  return target ? createPortal(content, target) : content;
}
