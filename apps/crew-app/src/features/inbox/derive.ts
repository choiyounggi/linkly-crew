import type { Envelope, TaskStateDto } from "../../lib/types";

/** 계약 E9 verbatim. */
export interface PendingGate {
  taskId: string;
  reason: string;
  ts: string;
  msgSeq: number;
}

type StoredMessage = { seq: number; envelope: Envelope };

function extractTaskId(body: unknown): string {
  if (typeof body !== "object" || body === null || !("task_id" in body)) return "";
  const taskId = (body as { task_id: unknown }).task_id;
  return typeof taskId === "string" ? taskId : "";
}

function extractReason(body: unknown): string {
  if (typeof body !== "object" || body === null || !("reason" in body)) return "";
  const reason = (body as { reason: unknown }).reason;
  return typeof reason === "string" ? reason : "";
}

/**
 * Pure derivation per contracts-m7.md §E9. A `human.gate` message counts only
 * when its task's current state is `"escalated"` — the state is the source
 * of truth, independent of any later `human.response`. Gates missing a
 * `task_id` are included with `taskId: ""` (display-only, no state to check).
 */
export function derivePendingGates(
  messages: StoredMessage[],
  taskStates: Record<string, TaskStateDto>,
): PendingGate[] {
  const gates: PendingGate[] = [];

  for (const { seq, envelope } of messages) {
    if (envelope.kind !== "human.gate") continue;
    const taskId = extractTaskId(envelope.body);
    if (taskId !== "" && taskStates[taskId] !== "escalated") continue;

    gates.push({
      taskId,
      reason: extractReason(envelope.body),
      ts: envelope.ts,
      msgSeq: seq,
    });
  }

  return gates.sort((a, b) => a.msgSeq - b.msgSeq);
}
