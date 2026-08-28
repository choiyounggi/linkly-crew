import { describe, expect, it } from "vitest";

import { groupThread } from "./derive";
import type { Envelope } from "../../lib/types";

function envelope(
  overrides: Partial<Envelope> & Pick<Envelope, "id" | "kind" | "from" | "corr">,
): Envelope {
  return {
    ts: "t",
    sprint: "sprint-1",
    thread: "t-pm",
    to: ["x"],
    in_reply_to: undefined,
    body: {},
    artifacts: [],
    requires_ack: false,
    deadline_ms: 1000,
    ...overrides,
  };
}

describe("groupThread — normal", () => {
  it("folds an ack into the message its in_reply_to points at", () => {
    const assign = envelope({ id: "e1", kind: "task.assign", from: "lead", corr: "t-pm" });
    const ack = envelope({
      id: "e2",
      kind: "task.ack",
      from: "agent:pm",
      corr: "t-pm",
      in_reply_to: "e1",
    });

    const items = groupThread([
      { seq: 1, envelope: assign },
      { seq: 2, envelope: ack },
    ]);

    expect(items).toHaveLength(1);
    expect(items[0].envelope.id).toBe("e1");
    expect(items[0].ackBy).toEqual(["agent:pm"]);
  });

  it("falls back to the last non-ack message in the same corr when in_reply_to is absent", () => {
    const assign = envelope({ id: "e1", kind: "task.assign", from: "lead", corr: "t-pm" });
    const ack = envelope({ id: "e2", kind: "task.ack", from: "agent:pm", corr: "t-pm" });

    const items = groupThread([
      { seq: 1, envelope: assign },
      { seq: 2, envelope: ack },
    ]);

    expect(items).toHaveLength(1);
    expect(items[0].ackBy).toEqual(["agent:pm"]);
  });

  it("accumulates multiple acks onto the same target, preserving order", () => {
    const assign = envelope({ id: "e1", kind: "task.assign", from: "lead", corr: "t-pm" });
    const ack1 = envelope({
      id: "e2",
      kind: "task.ack",
      from: "agent:pm",
      corr: "t-pm",
      in_reply_to: "e1",
    });
    const ack2 = envelope({
      id: "e3",
      kind: "task.ack",
      from: "agent:qa",
      corr: "t-pm",
      in_reply_to: "e1",
    });

    const items = groupThread([
      { seq: 1, envelope: assign },
      { seq: 2, envelope: ack1 },
      { seq: 3, envelope: ack2 },
    ]);

    expect(items).toHaveLength(1);
    expect(items[0].ackBy).toEqual(["agent:pm", "agent:qa"]);
  });

  it("keeps non-ack messages in seq order, unaffected by folding", () => {
    const assign = envelope({ id: "e1", kind: "task.assign", from: "lead", corr: "t-pm" });
    const ack = envelope({
      id: "e2",
      kind: "task.ack",
      from: "agent:pm",
      corr: "t-pm",
      in_reply_to: "e1",
    });
    const result = envelope({
      id: "e3",
      kind: "task.result",
      from: "agent:pm",
      corr: "t-pm",
      in_reply_to: "e2",
    });

    const items = groupThread([
      { seq: 1, envelope: assign },
      { seq: 2, envelope: ack },
      { seq: 3, envelope: result },
    ]);

    expect(items.map((i) => i.envelope.id)).toEqual(["e1", "e3"]);
  });
});

describe("groupThread — boundary", () => {
  it("returns an empty list for no messages", () => {
    expect(groupThread([])).toEqual([]);
  });

  it("keeps a dangling ack (no in_reply_to match, no prior message in its corr) as its own item", () => {
    const ack = envelope({ id: "e1", kind: "task.ack", from: "agent:pm", corr: "t-pm" });

    const items = groupThread([{ seq: 1, envelope: ack }]);

    expect(items).toHaveLength(1);
    expect(items[0].envelope.kind).toBe("task.ack");
    expect(items[0].ackBy).toEqual([]);
  });
});
