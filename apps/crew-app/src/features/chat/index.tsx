// contract: t7-fe-chat owns the implementation
// Stable surface consumed by t6-fe-shell: the channel's center chat pane.
// Assembles the slack-shaped stream (D1/D2/D8), the typing indicator (D5),
// the gate-response composer (D6), the right-side thread panel (D1
// exception, Task 03), and channel search (D9, Task 03).

import { useMemo, useState } from "react";

import { useRunStore } from "../../lib/store";
import ChatStream from "./ChatStream";
import Composer from "./Composer";
import { findActiveGate } from "./derive";
import { EMPTY_MESSAGES, EMPTY_TYPING } from "./empty";
import SearchOverlay from "./SearchOverlay";
import ThreadPanel from "./ThreadPanel";
import "./chat.css";

export interface ChatPaneProps {
  runId: string;
}

function TypingIndicator({ typing }: { typing: Record<string, boolean> }) {
  const active = Object.entries(typing)
    .filter(([, isTyping]) => isTyping)
    .map(([agentId]) => agentId);
  if (active.length === 0) return null;
  return (
    <p className="chat-pane__typing" aria-live="polite">
      {`${active.join(", ")} 입력 중...`}
    </p>
  );
}

/** Field-only store scope (plan Task 03) — no dedicated store action, so ChatPane sets `selectedThread` directly via `useRunStore.setState`. A missing channel (already closed) is a safe no-op. */
function setSelectedThread(runId: string, threadId: string | null) {
  useRunStore.setState((s) => {
    const channel = s.channels[runId];
    if (!channel) return s;
    return { channels: { ...s.channels, [runId]: { ...channel, selectedThread: threadId } } };
  });
}

export default function ChatPane({ runId }: ChatPaneProps) {
  const messages = useRunStore((s) => s.channels[runId]?.messages ?? EMPTY_MESSAGES);
  const typing = useRunStore((s) => s.channels[runId]?.typing ?? EMPTY_TYPING);
  const selectedThread = useRunStore((s) => s.channels[runId]?.selectedThread ?? null);
  const activeGate = useMemo(() => findActiveGate(messages), [messages]);
  const [searchOpen, setSearchOpen] = useState(false);

  function openThread(threadId: string) {
    setSelectedThread(runId, threadId);
  }

  return (
    <section className="chat-pane" aria-label="채널 대화">
      <div className="chat-pane__toolbar">
        <button type="button" className="chat-pane__search-trigger" onClick={() => setSearchOpen(true)}>
          검색
        </button>
      </div>
      <ChatStream runId={runId} onOpenThread={openThread} />
      <TypingIndicator typing={typing} />
      <Composer runId={runId} activeGate={activeGate} />
      {selectedThread && (
        <ThreadPanel runId={runId} threadId={selectedThread} onClose={() => setSelectedThread(runId, null)} />
      )}
      {searchOpen && (
        <SearchOverlay
          runId={runId}
          onClose={() => setSearchOpen(false)}
          onOpenThread={(threadId) => {
            openThread(threadId);
            setSearchOpen(false);
          }}
        />
      )}
    </section>
  );
}
