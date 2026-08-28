import type { Envelope } from "../../lib/types";

export interface ThreadItem {
  seq: number;
  envelope: Envelope;
  /** `from` of each task.ack folded into this item (§C5: no standalone ack row). */
  ackBy: string[];
}

/**
 * D1: folds each `task.ack` into the message it acknowledges instead of
 * emitting it as its own row. Target = the message `in_reply_to` points at,
 * falling back to the last non-ack message seen so far in the same `corr`.
 * An ack that matches neither (out-of-order/dangling) is kept as its own
 * item rather than dropped, so no message silently disappears.
 */
export function groupThread(messages: { seq: number; envelope: Envelope }[]): ThreadItem[] {
  const items: ThreadItem[] = [];
  const byId = new Map<string, ThreadItem>();
  const lastNonAckByCorr = new Map<string, ThreadItem>();

  for (const { seq, envelope } of messages) {
    if (envelope.kind === "task.ack") {
      const target =
        (envelope.in_reply_to !== undefined ? byId.get(envelope.in_reply_to) : undefined) ??
        lastNonAckByCorr.get(envelope.corr);
      if (target) {
        target.ackBy.push(envelope.from);
        continue;
      }
    }

    const item: ThreadItem = { seq, envelope, ackBy: [] };
    items.push(item);
    byId.set(envelope.id, item);
    if (envelope.kind !== "task.ack") {
      lastNonAckByCorr.set(envelope.corr, item);
    }
  }

  return items;
}
