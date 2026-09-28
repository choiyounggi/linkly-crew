// Thread-side answer widget (t8 plan D3d): there is no backend free-message
// command, so the composer is gate-response-only. It requires an active gate
// to mount at all — the caller (ThreadPanel) is responsible for not mounting
// it when there is nothing to answer, so this component never shows a
// disabled placeholder state; a control with nothing to do is absent, not
// disabled.

import { useState } from "react";

import { Button } from "../../components/primitives";
import { defaultSource } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import type { ActiveGate } from "./derive";

interface ComposerProps {
  runId: string;
  activeGate: ActiveGate;
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

  if (typeof source.resolveGate !== "function") return null;

  async function decide(decision: "approve" | "reject") {
    if (typeof source.resolveGate !== "function") return;
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
        value={reason}
        onChange={(e) => setReason(e.target.value)}
        disabled={submitting !== null}
        placeholder="승인/반려 사유 (선택)"
        aria-label="게이트 응답"
        rows={2}
      />
      <div className="composer__actions">
        <Button
          variant="primary"
          size="sm"
          disabled={submitting !== null}
          loading={submitting === "approve"}
          onClick={() => void decide("approve")}
        >
          승인
        </Button>
        <Button
          variant="danger"
          size="sm"
          disabled={submitting !== null}
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
