import { describe, expect, it } from "vitest";

import { ackReaders, deriveRoots, findActiveGate, gateResolutions, readersFor, unresolvedGateThreads } from "./derive";
import type { Envelope } from "../../lib/types";

function env(overrides: Partial<Envelope>): Envelope {
  return {
    id: "env_1",
    ts: "t",
    sprint: "sprint-1",
    thread: "env_1",
    from: "lead",
    to: ["agent:pm"],
    kind: "task.assign",
    corr: "env_1",
    body: {},
    artifacts: [],
    requires_ack: false,
    deadline_ms: 1000,
    ...overrides,
  };
}

describe("deriveRoots — normal (D1)", () => {
  it("keeps each root and folds its replies into replyCount", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "env_1", thread: "env_1", kind: "task.assign" }) },
      { seq: 2, envelope: env({ id: "env_2", thread: "env_1", kind: "task.ack" }) },
      { seq: 3, envelope: env({ id: "env_3", thread: "env_1", kind: "task.result" }) },
      { seq: 4, envelope: env({ id: "env_4", thread: "env_4", kind: "task.assign" }) },
    ];

    const roots = deriveRoots(messages);

    expect(roots).toHaveLength(2);
    expect(roots[0].envelope.id).toBe("env_1");
    expect(roots[0].replyCount).toBe(2);
    expect(roots[1].envelope.id).toBe("env_4");
    expect(roots[1].replyCount).toBe(0);
  });
});

describe("deriveRoots — boundary", () => {
  it("returns an empty list for no messages", () => {
    expect(deriveRoots([])).toEqual([]);
  });

  it("treats a lone reply (no root ever arrives) as its own provisional root with 0 replies", () => {
    const messages = [{ seq: 1, envelope: env({ id: "env_2", thread: "env_1", kind: "task.ack" }) }];

    const roots = deriveRoots(messages);

    expect(roots).toHaveLength(1);
    expect(roots[0].envelope.id).toBe("env_2");
    expect(roots[0].replyCount).toBe(0);
  });

  it("promotes the real root to replace a provisional one that arrived first (out-of-order), carrying the reply forward", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "env_2", thread: "env_1", kind: "task.ack" }) },
      { seq: 2, envelope: env({ id: "env_1", thread: "env_1", kind: "task.assign" }) },
    ];

    const roots = deriveRoots(messages);

    expect(roots).toHaveLength(1);
    expect(roots[0].envelope.id).toBe("env_1");
    expect(roots[0].seq).toBe(1);
    expect(roots[0].replyCount).toBe(1);
  });
});

describe("deriveRoots — human.gate folds into replyCount like any other reply (D3a — REPLACES the old main-stream-promotion exception)", () => {
  it("a human.gate reply folds into its thread root's replyCount, producing exactly 1 root", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "env_1", thread: "t-qa", kind: "task.assign" }) },
      { seq: 2, envelope: env({ id: "env_2", thread: "t-qa", kind: "blocked" }) },
      {
        seq: 3,
        envelope: env({ id: "env_3", thread: "t-qa", kind: "human.gate", body: { task_id: "t-qa", reason: "why" } }),
      },
    ];

    const roots = deriveRoots(messages);

    expect(roots).toHaveLength(1);
    expect(roots[0].envelope.id).toBe("env_1");
    expect(roots[0].replyCount).toBe(2); // blocked + human.gate both fold in (matches the pre-existing root's own reply count, unaffected by removing the promotion)
  });
});

describe("unresolvedGateThreads — normal/boundary (D3b)", () => {
  it("includes a thread whose human.gate has no matching human.response yet (normal)", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "g1", thread: "t-qa", kind: "human.gate", body: { task_id: "t-qa", reason: "why" } }) },
    ];

    expect(unresolvedGateThreads(messages)).toEqual(new Set(["t-qa"]));
  });

  it("excludes a thread whose gate has already been resolved (boundary)", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "g1", thread: "t-qa", kind: "human.gate", body: { task_id: "t-qa", reason: "why" } }) },
      {
        seq: 2,
        envelope: env({
          id: "r1",
          thread: "t-qa",
          kind: "human.response",
          body: { task_id: "t-qa", decision: "approve", reason: "ok" },
        }),
      },
    ];

    expect(unresolvedGateThreads(messages)).toEqual(new Set());
  });

  it("returns an empty set for no messages (boundary)", () => {
    expect(unresolvedGateThreads([])).toEqual(new Set());
  });
});

describe("ackReaders — normal/boundary (D2)", () => {
  it("adds the acker to the readers of the id named by in_reply_to (normal)", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "env_1", thread: "t-pm", kind: "task.assign" }) },
      { seq: 2, envelope: env({ id: "env_2", thread: "t-pm", kind: "task.ack", from: "agent:pm", in_reply_to: "env_1" }) },
    ];

    expect(ackReaders(messages)).toEqual({ env_1: ["agent:pm"] });
  });

  it("falls back to the thread's real ROOT ROW id (not the thread id string) when in_reply_to is absent, on a multi-message thread whose root id != thread id (normal/corrected)", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "env_1", thread: "t-pm", kind: "task.assign" }) },
      { seq: 2, envelope: env({ id: "env_2", thread: "t-pm", kind: "blocked" }) },
      { seq: 3, envelope: env({ id: "env_3", thread: "t-pm", kind: "task.ack", from: "agent:pm" }) },
    ];

    const readers = ackReaders(messages);

    expect(readers["env_1"]).toEqual(["agent:pm"]);
    expect(readers["t-pm"]).toBeUndefined();
  });

  it("records an ack whose in_reply_to matches no message id harmlessly, without throwing (boundary)", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "env_1", thread: "t-pm", kind: "task.assign" }) },
      {
        seq: 2,
        envelope: env({ id: "env_2", thread: "t-pm", kind: "task.ack", from: "agent:pm", in_reply_to: "no-such-id" }),
      },
    ];

    expect(() => ackReaders(messages)).not.toThrow();
    expect(ackReaders(messages)).toEqual({ "no-such-id": ["agent:pm"] });
  });
});

describe("readersFor — normal/boundary (D2)", () => {
  it("unions store read receipts and ack-derived readers, deduped (normal)", () => {
    const readers = readersFor("env_1", { env_1: ["agent:pm"] }, { env_1: ["agent:pm", "agent:qa"] });

    expect(readers.sort()).toEqual(["agent:pm", "agent:qa"]);
  });

  it("returns an empty array when both sources are empty for that id (boundary)", () => {
    expect(readersFor("env_1", {}, {})).toEqual([]);
  });
});

describe("gateResolutions — normal/boundary (D3)", () => {
  it("maps a task id to its parsed human.response", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "g1", thread: "t-qa", kind: "human.gate", body: { task_id: "t-qa", reason: "why" } }) },
      {
        seq: 2,
        envelope: env({
          id: "r1",
          thread: "t-qa",
          kind: "human.response",
          body: { task_id: "t-qa", decision: "approve", reason: "ok" },
        }),
      },
    ];

    expect(gateResolutions(messages)).toEqual(new Map([["t-qa", { task_id: "t-qa", decision: "approve", reason: "ok" }]]));
  });

  it("returns an empty map when there are no human.response messages", () => {
    expect(gateResolutions([])).toEqual(new Map());
  });
});

describe("findActiveGate — normal/error/boundary (D6)", () => {
  it("returns the open gate when no response has arrived yet (normal)", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "g1", thread: "t-qa", kind: "human.gate", body: { task_id: "t-qa", reason: "why" } }) },
    ];

    expect(findActiveGate(messages)).toEqual({ taskId: "t-qa", reason: "why" });
  });

  it("returns null once the matching human.response arrives (normal)", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "g1", thread: "t-qa", kind: "human.gate", body: { task_id: "t-qa", reason: "why" } }) },
      {
        seq: 2,
        envelope: env({
          id: "r1",
          thread: "t-qa",
          kind: "human.response",
          body: { task_id: "t-qa", decision: "reject", reason: "no" },
        }),
      },
    ];

    expect(findActiveGate(messages)).toBeNull();
  });

  it("returns null for no messages (boundary)", () => {
    expect(findActiveGate([])).toBeNull();
  });

  it("ignores a malformed human.gate body instead of throwing (error/boundary)", () => {
    const messages = [{ seq: 1, envelope: env({ id: "g1", thread: "t-qa", kind: "human.gate", body: {} }) }];

    expect(() => findActiveGate(messages)).not.toThrow();
    expect(findActiveGate(messages)).toBeNull();
  });
});
