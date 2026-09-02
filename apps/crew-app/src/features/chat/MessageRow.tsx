// One stream row (t7 plan D2): avatar/from/timestamp header, then a body
// rendered by a small kind -> renderer map. A kind with no map entry
// (including a genuinely unrecognized future kind) falls back to raw JSON,
// collapsed, so nothing crashes and nothing is silently hidden.

import { memo } from "react";

import type { Envelope, MessageKind } from "../../lib/types";
import type { HumanResponseBody } from "./body";
import { parseChangeRequestBody, parseHumanGateBody, parseTaskResultBody } from "./body";
import GateCard from "./GateCard";

interface MessageRowProps {
  runId: string;
  envelope: Envelope;
  replyCount: number;
  readers: string[];
  gateResolution: HumanResponseBody | null;
  /** t7 plan D1 exception (R3): set when this row is a `human.gate` promoted into the main stream — names the task thread it belongs to. */
  parentThread?: string;
  onOpenThread?: (threadId: string) => void;
}

function formatTs(ts: string): string {
  const d = new Date(ts);
  return Number.isNaN(d.getTime()) ? ts : d.toLocaleTimeString();
}

function rowVariantClass(kind: MessageKind): string {
  if (kind === "change_request") return "message-row--danger";
  if (kind === "human.gate") return "message-row--warning";
  return "";
}

function RawBody({ body }: { body: unknown }) {
  return (
    <details className="message-body__raw">
      <summary>{"본문 보기"}</summary>
      <pre>{JSON.stringify(body, null, 2)}</pre>
    </details>
  );
}

function MessageBody({ runId, envelope, gateResolution }: Pick<MessageRowProps, "runId" | "envelope" | "gateResolution">) {
  if (envelope.kind === "task.result") {
    const parsed = parseTaskResultBody(envelope.body);
    if (parsed) {
      return (
        <div className="message-body">
          <p className="message-body__req">커버: {parsed.covered_req_ids.join(", ") || "—"}</p>
          {parsed.artifacts.length > 0 && (
            <ul className="message-body__artifacts">
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
        <div className="message-body">
          <p>위반: {parsed.violations.join(", ") || "—"}</p>
          <p>{parsed.reason}</p>
        </div>
      );
    }
  }

  if (envelope.kind === "human.gate") {
    const parsed = parseHumanGateBody(envelope.body);
    if (parsed) {
      return <GateCard runId={runId} taskId={parsed.task_id} reason={parsed.reason} resolution={gateResolution} />;
    }
  }

  return <RawBody body={envelope.body} />;
}

function MessageRow({ runId, envelope, replyCount, readers, gateResolution, parentThread, onOpenThread }: MessageRowProps) {
  const variant = rowVariantClass(envelope.kind);
  const initial = envelope.from.replace(/^agent:/, "").charAt(0).toUpperCase() || "?";

  return (
    <li className={variant ? `message-row ${variant}` : "message-row"}>
      <div className="message-row__avatar" aria-hidden="true">
        {initial}
      </div>
      <div className="message-row__main">
        <div className="message-row__meta">
          <span className="message-row__from">{envelope.from}</span>
          <span className="message-row__kind">{envelope.kind}</span>
          <time className="message-row__ts" dateTime={envelope.ts}>
            {formatTs(envelope.ts)}
          </time>
        </div>
        <MessageBody runId={runId} envelope={envelope} gateResolution={gateResolution} />
        <div className="message-row__footer">
          {readers.length > 0 && (
            <span className="message-row__readers" title={readers.join(", ")}>
              {`👀 ${readers.length}`}
            </span>
          )}
          {parentThread &&
            (onOpenThread ? (
              <button type="button" className="message-row__parent-thread" onClick={() => onOpenThread(parentThread)}>
                {`스레드: ${parentThread}`}
              </button>
            ) : (
              <span className="message-row__parent-thread">{`스레드: ${parentThread}`}</span>
            ))}
          {replyCount > 0 &&
            (onOpenThread ? (
              <button type="button" className="message-row__replies" onClick={() => onOpenThread(envelope.thread)}>
                {`댓글 ${replyCount}개`}
              </button>
            ) : (
              <span className="message-row__replies">{`댓글 ${replyCount}개`}</span>
            ))}
        </div>
      </div>
    </li>
  );
}

/** t7 plan D8: memoized so a presence/message update elsewhere in the channel doesn't re-render every row. */
export default memo(MessageRow);
