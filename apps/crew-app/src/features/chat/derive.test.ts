import { describe, expect, it } from "vitest";

import { deriveRoots, findActiveGate, gateResolutions } from "./derive";
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

describe("deriveRoots — human.gate main-stream exception (D1/R3)", () => {
  it("promotes a human.gate into its own root while still counting it as a reply on its real thread", () => {
    const messages = [
      { seq: 1, envelope: env({ id: "env_1", thread: "t-qa", kind: "task.assign" }) },
      { seq: 2, envelope: env({ id: "env_2", thread: "t-qa", kind: "blocked" }) },
      {
        seq: 3,
        envelope: env({ id: "env_3", thread: "t-qa", kind: "human.gate", body: { task_id: "t-qa", reason: "why" } }),
      },
    ];

    const roots = deriveRoots(messages);

    expect(roots).toHaveLength(2);
    expect(roots[0].envelope.id).toBe("env_1");
    expect(roots[0].replyCount).toBe(2); // blocked + human.gate both fold in
    expect(roots[1].envelope.id).toBe("env_3");
    expect(roots[1].envelope.kind).toBe("human.gate");
    expect(roots[1].replyCount).toBe(0);
    expect(roots[1].parentThread).toBe("t-qa");
  });

  it("does not duplicate a human.gate that is already its thread's own root (boundary)", () => {
    const messages = [
      {
        seq: 1,
        envelope: env({ id: "env_1", thread: "env_1", kind: "human.gate", body: { task_id: "env_1", reason: "why" } }),
      },
    ];

    const roots = deriveRoots(messages);

    expect(roots).toHaveLength(1);
    expect(roots[0].envelope.id).toBe("env_1");
    expect(roots[0].parentThread).toBeUndefined();
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
