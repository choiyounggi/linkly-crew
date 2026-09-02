import type { Envelope } from "../../lib/types";
import { parseHumanGateBody, parseHumanResponseBody, type HumanResponseBody } from "./body";

export interface ChatRootItem {
  seq: number;
  envelope: Envelope;
  replyCount: number;
  /** t7 plan D1 exception (R3): set when this root is a `human.gate` promoted into the main stream even though it is a reply, not its thread's actual root — names the task thread it still belongs to (also `envelope.thread`), so a click can still open that thread. */
  parentThread?: string;
}

/**
 * t7 plan D1: only thread roots stream (a message is a root when its
 * `id === thread`, or — for a reply that arrives before its root, e.g.
 * out-of-order/scripted demo delivery — it is the first message seen for
 * that `thread`, provisionally, until the real root shows up). Every other
 * message in the same thread folds into the root's `replyCount` ("댓글 N개").
 *
 * D1 exception (R3, decisions.md): a `human.gate` always ALSO surfaces as
 * its own one-message root in the main stream — approval must never be
 * buried behind a "댓글 N개" click — while still counting as a reply on its
 * real thread's root. Skipped only when the gate happens to already be that
 * thread's actual root (no duplicate row).
 */
export function deriveRoots(messages: { seq: number; envelope: Envelope }[]): ChatRootItem[] {
  const rootByThread = new Map<string, ChatRootItem>();
  const order: string[] = [];

  for (const { seq, envelope } of messages) {
    const key = envelope.thread;
    const existing = rootByThread.get(key);
    if (!existing) {
      rootByThread.set(key, { seq, envelope, replyCount: 0 });
      order.push(key);
    } else if (envelope.id === key) {
      rootByThread.set(key, { seq: existing.seq, envelope, replyCount: existing.replyCount + 1 });
    } else {
      existing.replyCount += 1;
    }

    if (envelope.kind === "human.gate" && rootByThread.get(key)?.envelope.id !== envelope.id) {
      const gateKey = `gate:${envelope.id}`;
      rootByThread.set(gateKey, { seq, envelope, replyCount: 0, parentThread: key });
      order.push(gateKey);
    }
  }

  return order.map((key) => {
    const item = rootByThread.get(key);
    if (!item) throw new Error(`deriveRoots: missing root for key "${key}"`);
    return item;
  });
}

/** t7 plan D3: task id -> its `human.response`, once one arrives — GateCard reads this to disable itself and show the outcome. */
export function gateResolutions(messages: { seq: number; envelope: Envelope }[]): Map<string, HumanResponseBody> {
  const resolved = new Map<string, HumanResponseBody>();
  for (const { envelope } of messages) {
    if (envelope.kind !== "human.response") continue;
    const parsed = parseHumanResponseBody(envelope.body);
    if (parsed) resolved.set(parsed.task_id, parsed);
  }
  return resolved;
}

export interface ActiveGate {
  taskId: string;
  reason: string;
}

/** t7 plan D6: the composer targets the most recent `human.gate` that has no `human.response` yet, or null when none is active. */
export function findActiveGate(messages: { seq: number; envelope: Envelope }[]): ActiveGate | null {
  const open = new Map<string, string>();
  for (const { envelope } of messages) {
    if (envelope.kind === "human.gate") {
      const parsed = parseHumanGateBody(envelope.body);
      if (parsed) open.set(parsed.task_id, parsed.reason);
      continue;
    }
    if (envelope.kind === "human.response") {
      const parsed = parseHumanResponseBody(envelope.body);
      if (parsed) open.delete(parsed.task_id);
    }
  }
  const last = [...open.entries()].at(-1);
  return last ? { taskId: last[0], reason: last[1] } : null;
}
