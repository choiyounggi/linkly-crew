// Bottom input (t7 plan D6): there is no backend free-message command, so
// the composer is gate-response-only. With an active gate it lets the user
// type a reason and approve/reject it (same resolveGate(runId, ...) call
// GateCard uses); with none, it is disabled with a placeholder explaining
// why (free text is a later milestone).

import { useState } from "react";

import { Button } from "../../components/primitives";
import { defaultSource } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import type { ActiveGate } from "./derive";

interface ComposerProps {
  runId: string;
  activeGate: ActiveGate | null;
  source?: RunEventSource;
}

type Submitting = "approve" | "reject" | null;

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export default function Composer({ runId, activeGate, source = defaultSource }: ComposerProps) {
  const [reason, setReason] = useState("");
  const [submitting, setSubmitting] = useState<Submitting>(null);
  const [error, setError] = useState<string | null>(null);

  const active = activeGate !== null && typeof source.resolveGate === "function";

  async function decide(decision: "approve" | "reject") {
    if (!activeGate || typeof source.resolveGate !== "function") return;
    setSubmitting(decision);
    setError(null);
    try {
      await source.resolveGate(runId, activeGate.taskId, decision, reason);
      setReason("");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSubmitting(null);
    }
  }

  return (
    <div className="composer">
      <textarea
        className="composer__input"
        value={active ? reason : ""}
        onChange={(e) => setReason(e.target.value)}
        disabled={!active || submitting !== null}
        placeholder={active ? "승인/반려 사유 (선택)" : "@멘션 2차 예정"}
        aria-label="게이트 응답"
        rows={2}
      />
      <div className="composer__actions">
        <Button
          variant="primary"
          size="sm"
          disabled={!active || submitting !== null}
          loading={submitting === "approve"}
          onClick={() => void decide("approve")}
        >
          승인
        </Button>
        <Button
          variant="danger"
          size="sm"
          disabled={!active || submitting !== null}
          loading={submitting === "reject"}
          onClick={() => void decide("reject")}
        >
          반려
        </Button>
      </div>
      {error && (
        <p className="composer__error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
