import { useEffect, useRef } from "react";

import { useRunStore } from "../../lib/store";
import type { Envelope, MessageKind } from "../../lib/types";
import { parseChangeRequestBody, parseTaskResultBody } from "./body";
import { groupThread, type ThreadItem } from "./derive";
import "./thread.css";

const STICK_TO_BOTTOM_THRESHOLD_PX = 40;

function formatTs(ts: string): string {
  const d = new Date(ts);
  return Number.isNaN(d.getTime()) ? ts : d.toLocaleTimeString();
}

function rowVariantClass(kind: MessageKind): string {
  if (kind === "change_request") return "thread-row--danger";
  if (kind === "human.gate") return "thread-row--warning";
  return "";
}

function kindBadgeClass(kind: MessageKind): string {
  if (kind === "change_request") return "thread-badge thread-badge--danger";
  if (kind === "human.gate") return "thread-badge thread-badge--warning";
  return "thread-badge";
}

function MessageBody({ envelope }: { envelope: Envelope }) {
  if (envelope.kind === "task.result") {
    const parsed = parseTaskResultBody(envelope.body);
    if (parsed) {
      return (
        <div className="thread-body">
          <p className="thread-body__req">커버: {parsed.covered_req_ids.join(", ") || "—"}</p>
          {parsed.artifacts.length > 0 && (
            <ul className="thread-body__artifacts">
              {parsed.artifacts.map((artifact, index) => (
                <li key={`${artifact.name}-${index}`}>
                  <details>
                    <summary>{artifact.name}</summary>
                    <pre>{artifact.content}</pre>
                  </details>
                </li>
              ))}
            </ul>
          )}
        </div>
      );
    }
  }

  if (envelope.kind === "change_request") {
    const parsed = parseChangeRequestBody(envelope.body);
    if (parsed) {
      return (
        <div className="thread-body">
          <p>위반: {parsed.violations.join(", ") || "—"}</p>
          <p>{parsed.reason}</p>
        </div>
      );
    }
  }

  return <pre className="thread-body__raw">{JSON.stringify(envelope.body)}</pre>;
}

function ThreadRow({ item }: { item: ThreadItem }) {
  const { envelope, ackBy } = item;
  const variant = rowVariantClass(envelope.kind);

  return (
    <li className={variant ? `thread-row ${variant}` : "thread-row"}>
      <div className="thread-row__meta">
        <span className="thread-row__parties">
          {envelope.from} → {envelope.to.join(", ")}
        </span>
        <span className={kindBadgeClass(envelope.kind)}>{envelope.kind}</span>
        {ackBy.map((from) => (
          <span key={from} className="thread-badge thread-badge--ack">{`👀 ${from}`}</span>
        ))}
        <time className="thread-row__ts" dateTime={envelope.ts}>
          {formatTs(envelope.ts)}
        </time>
      </div>
      <MessageBody envelope={envelope} />
    </li>
  );
}

export default function Thread() {
  // Mechanical adaptation to the multi-run store (plan D1) — reads the
  // active channel's messages instead of the old flat field. No
  // behavior/UI change; this file otherwise stays t7's call.
  const messages = useRunStore((s) => (s.activeRunId ? (s.channels[s.activeRunId]?.messages ?? []) : []));
  const items = groupThread(messages);

  const sectionRef = useRef<HTMLElement | null>(null);
  const stickToBottomRef = useRef(true);

  const handleScroll = () => {
    const el = sectionRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickToBottomRef.current = distanceFromBottom < STICK_TO_BOTTOM_THRESHOLD_PX;
  };

  // D4: keep the view pinned to the newest message unless the user has
  // scrolled up to read earlier ones — never force-jump on top of them.
  useEffect(() => {
    const el = sectionRef.current;
    if (!el || !stickToBottomRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [items.length]);

  return (
    <section
      className="panel panel--thread"
      aria-label="라이브 스레드"
      ref={sectionRef}
      onScroll={handleScroll}
    >
      <h2 className="panel__title">라이브 스레드</h2>
      {items.length === 0 ? (
        <p>아직 메시지가 없습니다</p>
      ) : (
        <ul className="thread-list">
          {items.map((item) => (
            <ThreadRow key={item.envelope.id} item={item} />
          ))}
        </ul>
      )}
    </section>
  );
}
