// One stream row (t7 plan D2): avatar/from/timestamp header, then a body
// rendered by a small kind -> renderer map. A kind with no map entry
// (including a genuinely unrecognized future kind) falls back to raw JSON,
// collapsed, so nothing crashes and nothing is silently hidden.

import { memo } from "react";

import type { Envelope, MessageKind } from "../../lib/types";
import type { HumanResponseBody } from "./body";
import {
  parseBlockedBody,
  parseChangeRequestBody,
  parseHandoffBody,
  parseHumanGateBody,
  parseHumanResponseBody,
  parseTaskAssignBody,
  parseTaskProgressBody,
  parseTaskResultBody,
  parseTextBody,
} from "./body";

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

function mentionLabel(recipient: string): string {
  return `@${recipient.replace(/^agent:/, "")}`;
}

function RawToggle({ body }: { body: unknown }) {
  return (
    <details className="message-body__raw">
      <summary>{"원문"}</summary>
      <pre>{JSON.stringify(body, null, 2)}</pre>
    </details>
  );
}

function mentionLabelForTemplate(recipient: string): string {
  return `@${recipient.replace(/^agent:/, "")}`;
}

function renderTaskAssign(envelope: Envelope): string {
  const parsed = parseTaskAssignBody(envelope.body);
  const who = envelope.to[0] ? mentionLabelForTemplate(envelope.to[0]) : null;
  if (parsed && who) return `${who}님, ${parsed.title} 진행해 주세요`;
  if (parsed) return `${parsed.title} 진행해 주세요`;
  if (who) return `${who}님, 작업을 진행해 주세요`;
  return "작업을 진행해 주세요";
}

function renderTaskProgress(envelope: Envelope): string {
  const parsed = parseTaskProgressBody(envelope.body);
  return parsed ? parsed.summary : "진행 상황이 업데이트되었습니다";
}

function renderReviewRequest(envelope: Envelope): string {
  const who = envelope.to[0] ? mentionLabelForTemplate(envelope.to[0]) : null;
  return who ? `${who}님 리뷰 요청` : "리뷰 요청";
}

function renderQuestion(envelope: Envelope): string {
  const parsed = parseTextBody(envelope.body);
  return `❓ ${parsed ? parsed.text : "(내용 없음)"}`;
}

function renderAnswer(envelope: Envelope): string {
  const parsed = parseTextBody(envelope.body);
  return `💬 ${parsed ? parsed.text : "(내용 없음)"}`;
}

function renderBlocked(envelope: Envelope): string {
  const parsed = parseBlockedBody(envelope.body);
  return parsed ? `🚧 차단: ${parsed.reason}` : "🚧 차단됨";
}

function renderHandoff(envelope: Envelope): string {
  const parsed = parseHandoffBody(envelope.body);
  return parsed ? `🔄 인수인계: ${parsed.role}` : "🔄 인수인계됨";
}

function renderHumanGate(envelope: Envelope): string {
  const parsed = parseHumanGateBody(envelope.body);
  return parsed ? `@human님, ${parsed.task_id} 검토가 필요합니다: ${parsed.reason}` : "@human님, 검토가 필요합니다";
}

function renderHumanResponse(envelope: Envelope): string {
  const parsed = parseHumanResponseBody(envelope.body);
  if (!parsed) return "응답이 도착했습니다";
  const verdict = parsed.decision === "approve" ? "승인됨" : "반려됨";
  return parsed.reason ? `${verdict} — ${parsed.reason}` : verdict;
}

function renderUnknownKind(envelope: Envelope): string {
  return `알 수 없는 메시지 종류: ${envelope.kind}`;
}

const KIND_LINE_RENDERERS: Partial<Record<MessageKind, (envelope: Envelope) => string>> = {
  "task.assign": renderTaskAssign,
  "task.progress": renderTaskProgress,
  "review.request": renderReviewRequest,
  question: renderQuestion,
  answer: renderAnswer,
  blocked: renderBlocked,
  handoff: renderHandoff,
  "human.gate": renderHumanGate,
  "human.response": renderHumanResponse,
};

function MessageBody({ envelope }: Pick<MessageRowProps, "envelope">) {
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
          <RawToggle body={envelope.body} />
        </div>
      );
    }
    return (
      <div className="message-body">
        <p>{"작업 결과가 도착했습니다 (본문 형식 오류)"}</p>
        <RawToggle body={envelope.body} />
      </div>
    );
  }

  if (envelope.kind === "change_request") {
    const parsed = parseChangeRequestBody(envelope.body);
    if (parsed) {
      return (
        <div className="message-body">
          <p>위반: {parsed.violations.join(", ") || "—"}</p>
          <p>{parsed.reason}</p>
          <RawToggle body={envelope.body} />
        </div>
      );
    }
    return (
      <div className="message-body">
        <p>{"변경 요청 (본문 형식 오류)"}</p>
        <RawToggle body={envelope.body} />
      </div>
    );
  }

  const line = KIND_LINE_RENDERERS[envelope.kind]?.(envelope) ?? renderUnknownKind(envelope);
  return (
    <div className="message-body">
      <p>{line}</p>
      <RawToggle body={envelope.body} />
    </div>
  );
}

function MessageRow({ envelope, replyCount, readers, parentThread, onOpenThread }: MessageRowProps) {
  if (envelope.kind === "task.ack") return null;

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
        <MessageBody envelope={envelope} />
        {envelope.to.length > 0 && (
          <div className="message-row__mentions">
            {envelope.to.map((recipient) =>
              onOpenThread ? (
                <button
                  key={recipient}
                  type="button"
                  className="message-row__mention-chip"
                  onClick={() => onOpenThread(envelope.thread)}
                >
                  {mentionLabel(recipient)}
                </button>
              ) : (
                <span key={recipient} className="message-row__mention-chip">
                  {mentionLabel(recipient)}
                </span>
              ),
            )}
          </div>
        )}
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
