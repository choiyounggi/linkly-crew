import { useState } from "react";

import { defaultSource, useRunStore } from "../../lib/store";
import { derivePendingGates, type PendingGate } from "./derive";
import "./inbox.css";

type Decision = "approve" | "reject";

function GateItem({ gate }: { gate: PendingGate }) {
  const [reason, setReason] = useState("");
  const [pending, setPending] = useState<Decision | null>(null);
  const [error, setError] = useState<string | null>(null);

  const resolveGate = defaultSource.resolveGate;

  async function handleDecision(decision: Decision) {
    if (!resolveGate) return;
    setPending(decision);
    setError(null);
    try {
      await resolveGate(gate.taskId, decision, reason);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPending(null);
    }
  }

  return (
    <li className="inbox-item">
      <div className="inbox-item__meta">
        <span className="inbox-item__task-id">{gate.taskId || "(taskId 없음)"}</span>
        <span className="inbox-item__ts">{gate.ts}</span>
      </div>
      <p className="inbox-item__reason">{gate.reason}</p>
      <input
        className="inbox-item__reason-input"
        type="text"
        placeholder="사유 입력"
        value={reason}
        onChange={(e) => setReason(e.target.value)}
        disabled={pending !== null}
        aria-label="승인/반려 사유"
      />
      <div className="inbox-item__actions">
        {resolveGate ? (
          <>
            <button
              type="button"
              disabled={pending !== null || gate.taskId === ""}
              onClick={() => handleDecision("approve")}
            >
              승인
            </button>
            <button
              type="button"
              disabled={pending !== null || gate.taskId === ""}
              onClick={() => handleDecision("reject")}
            >
              반려
            </button>
          </>
        ) : (
          <span className="inbox-item__unsupported">이 소스에서 지원 안 함</span>
        )}
      </div>
      {error && <p className="inbox-item__error">{error}</p>}
    </li>
  );
}

export default function Inbox() {
  const messages = useRunStore((s) => s.messages);
  const taskStates = useRunStore((s) => s.taskStates);
  const gates = derivePendingGates(messages, taskStates);

  return (
    <section className="inbox-view" aria-label="승인함">
      {gates.length === 0 ? (
        <p className="inbox-empty">대기 중인 승인 요청이 없습니다</p>
      ) : (
        <ul className="inbox-list">
          {gates.map((gate) => (
            <GateItem key={gate.msgSeq} gate={gate} />
          ))}
        </ul>
      )}
    </section>
  );
}
