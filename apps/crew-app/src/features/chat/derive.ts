import type { Envelope } from "../../lib/types";
import { parseHumanGateBody, parseHumanResponseBody, type HumanResponseBody } from "./body";

export interface ChatRootItem {
  seq: number;
  envelope: Envelope;
  replyCount: number;
}

/**
 * t7 plan D1: only thread roots stream (a message is a root when its
 * `id === thread`, or — for a reply that arrives before its root, e.g.
 * out-of-order/scripted demo delivery — it is the first message seen for
 * that `thread`, provisionally, until the real root shows up). Every other
 * message in the same thread folds into the root's `replyCount` ("댓글 N개"),
 * including `human.gate` (t8 plan D3a — the prior main-stream promotion
 * exception is gone; an unresolved gate now surfaces via `unresolvedGateThreads`
 * below and a footer badge, not a duplicated row).
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
  }

  return order.map((key) => {
    const item = rootByThread.get(key);
    if (!item) throw new Error(`deriveRoots: missing root for key "${key}"`);
    return item;
  });
}

/** t8 plan D2: a `task.ack`'s sender is added to the readers of the message it acknowledges — `in_reply_to` when present, otherwise the ack's thread ROOT ROW's real `envelope.id` (never the thread id string itself; message ids and thread ids are disjoint namespaces). */
export function ackReaders(messages: { seq: number; envelope: Envelope }[]): Record<string, string[]> {
  const roots = deriveRoots(messages);
  const rootIdByThread = new Map(roots.map((r) => [r.envelope.thread, r.envelope.id]));
  const readers: Record<string, string[]> = {};
  for (const { envelope } of messages) {
    if (envelope.kind !== "task.ack") continue;
    const targetId = envelope.in_reply_to ?? rootIdByThread.get(envelope.thread) ?? envelope.thread;
    const existing = readers[targetId] ?? [];
    if (!existing.includes(envelope.from)) readers[targetId] = [...existing, envelope.from];
  }
  return readers;
}

/** t8 plan D2: unions the store's presence-derived read receipts with ack-derived readers for one message id, deduped. */
export function readersFor(
  id: string,
  storeReadReceipts: Record<string, string[]>,
  ackMap: Record<string, string[]>,
): string[] {
  const combined = new Set([...(storeReadReceipts[id] ?? []), ...(ackMap[id] ?? [])]);
  return [...combined];
}

/** t7 plan D3: task id -> its `human.response`, once one arrives — used to tell a resolved gate from an unresolved one. */
export function gateResolutions(messages: { seq: number; envelope: Envelope }[]): Map<string, HumanResponseBody> {
  const resolved = new Map<string, HumanResponseBody>();
  for (const { envelope } of messages) {
    if (envelope.kind !== "human.response") continue;
    const parsed = parseHumanResponseBody(envelope.body);
    if (parsed) resolved.set(parsed.task_id, parsed);
  }
  return resolved;
}

/** t8 plan D3b: every thread that holds a `human.gate` with no matching `human.response` yet — drives the "응답 필요" footer badge on that thread's root row. Computed at render time from `messages`, never stored. */
export function unresolvedGateThreads(messages: { seq: number; envelope: Envelope }[]): Set<string> {
  const resolved = gateResolutions(messages);
  const unresolved = new Set<string>();
  for (const { envelope } of messages) {
    if (envelope.kind !== "human.gate") continue;
    const parsed = parseHumanGateBody(envelope.body);
    if (!parsed) continue;
    if (!resolved.has(parsed.task_id)) unresolved.add(envelope.thread);
  }
  return unresolved;
}

export interface ActiveGate {
  taskId: string;
  reason: string;
}

/**
 * t7 plan D6: the composer targets the most recent `human.gate` that has no
 * `human.response` yet, or null when none is active.
 *
 * integ-fix F2: on the real backend `human.gate` goes out on the Lead's own
 * thread (`th-agent:lead`) while the matching `human.response` lands on a
 * different thread (`th-gate-<task_id>`, posted by the human proxy) — only
 * the mock and old fixtures ever put both on the same thread. So gate
 * candidates are restricted to `threadMessages` (this thread's own
 * `human.gate` rows) but a gate is closed by a `human.response` from
 * ANYWHERE in the run (`allMessages`, defaulting to `threadMessages` so
 * same-thread callers/tests are unaffected). Order stays seq-aware so a
 * re-escalation (a new `human.gate` for a task_id already resolved) reopens
 * the composer for it.
 */
export function findActiveGate(
  threadMessages: { seq: number; envelope: Envelope }[],
  allMessages: { seq: number; envelope: Envelope }[] = threadMessages,
): ActiveGate | null {
  const relevant = [
    ...threadMessages.filter(({ envelope }) => envelope.kind === "human.gate"),
    ...allMessages.filter(({ envelope }) => envelope.kind === "human.response"),
  ].sort((a, b) => a.seq - b.seq);

  const open = new Map<string, string>();
  for (const { envelope } of relevant) {
    if (envelope.kind === "human.gate") {
      const parsed = parseHumanGateBody(envelope.body);
      if (parsed) open.set(parsed.task_id, parsed.reason);
      continue;
    }
    const parsed = parseHumanResponseBody(envelope.body);
    if (parsed) open.delete(parsed.task_id);
  }
  const last = [...open.entries()].at(-1);
  return last ? { taskId: last[0], reason: last[1] } : null;
}
