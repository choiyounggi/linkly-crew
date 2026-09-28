// Root-only message stream (t7 plan D1/D8): renders thread roots with a
// reply-count badge, pinned to the newest message unless the user has
// scrolled up. Below a few hundred rows (a channel's realistic message
// count) direct rendering is correct per wiki/frontend/rendering/long-lists
// — virtualization is out of scope (D8).

import { useEffect, useMemo, useRef } from "react";

import { useRunStore } from "../../lib/store";
import { parseHumanGateBody } from "./body";
import { ackReaders, deriveRoots, gateResolutions, readersFor, unresolvedGateThreads } from "./derive";
import { EMPTY_MESSAGES, EMPTY_READ_RECEIPTS } from "./empty";
import MessageRow from "./MessageRow";

const STICK_TO_BOTTOM_THRESHOLD_PX = 40;

interface ChatStreamProps {
  runId: string;
  onOpenThread?: (threadId: string) => void;
}

export default function ChatStream({ runId, onOpenThread }: ChatStreamProps) {
  const messages = useRunStore((s) => s.channels[runId]?.messages ?? EMPTY_MESSAGES);
  const readReceipts = useRunStore((s) => s.channels[runId]?.readReceipts ?? EMPTY_READ_RECEIPTS);

  const roots = useMemo(() => deriveRoots(messages), [messages]);
  const resolutions = useMemo(() => gateResolutions(messages), [messages]);
  const ackMap = useMemo(() => ackReaders(messages), [messages]);
  const unresolvedThreads = useMemo(() => unresolvedGateThreads(messages), [messages]);

  const sectionRef = useRef<HTMLElement | null>(null);
  const stickToBottomRef = useRef(true);

  const handleScroll = () => {
    const el = sectionRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickToBottomRef.current = distanceFromBottom < STICK_TO_BOTTOM_THRESHOLD_PX;
  };

  useEffect(() => {
    const el = sectionRef.current;
    if (!el || !stickToBottomRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [roots.length]);

  return (
    <section className="chat-stream" aria-label="채널 메시지" ref={sectionRef} onScroll={handleScroll}>
      {roots.length === 0 ? (
        <p className="chat-stream__empty">아직 메시지가 없습니다</p>
      ) : (
        <ul className="chat-stream__list">
          {roots.map((root) => {
            const gateTaskId = root.envelope.kind === "human.gate" ? parseHumanGateBody(root.envelope.body)?.task_id : undefined;
            return (
              <MessageRow
                key={root.envelope.id}
                runId={runId}
                envelope={root.envelope}
                replyCount={root.replyCount}
                readers={readersFor(root.envelope.id, readReceipts, ackMap)}
                gateResolution={gateTaskId ? (resolutions.get(gateTaskId) ?? null) : null}
                unresolvedGate={unresolvedThreads.has(root.envelope.thread)}
                onOpenThread={onOpenThread}
              />
            );
          })}
        </ul>
      )}
    </section>
  );
}
