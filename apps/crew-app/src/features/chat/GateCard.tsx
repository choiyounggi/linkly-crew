// Inline gate approval card (t7 plan D3): renders a `human.gate` message's
// reason plus approve/reject buttons that call `resolveGate(runId, ...)`
// directly (no store action — mirrors RosterPanel/NewTaskModal's pattern of
// calling the injectable `source` for actions the multi-run store doesn't
// own). `resolution` is derived by the caller from the channel's messages
// (derive.ts's `gateResolutions`), never stored locally, so re-syncing the
// channel or another GateCard instance for the same task stays consistent.

import { useState } from "react";

import { Button } from "../../components/primitives";
import { defaultSource } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import type { HumanResponseBody } from "./body";

interface GateCardProps {
  runId: string;
  taskId: string;
  reason: string;
  resolution: HumanResponseBody | null;
  source?: RunEventSource;
}

type Submitting = "approve" | "reject" | null;

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export default function GateCard({ runId, taskId, reason, resolution, source = defaultSource }: GateCardProps) {
  const [submitting, setSubmitting] = useState<Submitting>(null);
  const [error, setError] = useState<string | null>(null);

  const disabled = resolution !== null || submitting !== null || typeof source.resolveGate !== "function";

  async function decide(decision: "approve" | "reject") {
    if (typeof source.resolveGate !== "function") return;
    setSubmitting(decision);
    setError(null);
    try {
      await source.resolveGate(runId, taskId, decision, reason);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSubmitting(null);
    }
  }

  return (
    <div className="gate-card" role="group" aria-label="게이트 승인">
      <p className="gate-card__reason">{reason}</p>
      {resolution ? (
        <p className="gate-card__resolution">
          {resolution.decision === "approve" ? "승인됨" : "반려됨"}
          {resolution.reason ? ` — ${resolution.reason}` : ""}
        </p>
      ) : (
        <div className="gate-card__actions">
          <Button variant="primary" size="sm" disabled={disabled} loading={submitting === "approve"} onClick={() => void decide("approve")}>
            승인
          </Button>
          <Button variant="danger" size="sm" disabled={disabled} loading={submitting === "reject"} onClick={() => void decide("reject")}>
            반려
          </Button>
        </div>
      )}
      {error && (
        <p className="gate-card__error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
