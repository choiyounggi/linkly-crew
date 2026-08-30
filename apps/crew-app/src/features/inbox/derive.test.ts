import { describe, expect, it } from "vitest";

import type { Envelope, TaskStateDto } from "../../lib/types";
import { derivePendingGates } from "./derive";

function makeGate(seq: number, body: unknown, ts = "2026-08-30T00:00:00Z"): { seq: number; envelope: Envelope } {
  return {
    seq,
    envelope: {
      id: `env_${seq}`,
      ts,
      sprint: "s1",
      thread: "t1",
      from: "lead",
      to: [],
      kind: "human.gate",
      corr: "t1",
      body,
      artifacts: [],
      requires_ack: false,
      deadline_ms: 60_000,
    },
  };
}

describe("derivePendingGates", () => {
  it("includes only human.gate messages whose task is currently escalated", () => {
    const messages = [
      makeGate(1, { task_id: "t-a", reason: "a 검토 필요" }),
      makeGate(2, { task_id: "t-b", reason: "b 검토 필요" }),
    ];
    const taskStates: Record<string, TaskStateDto> = { "t-a": "escalated", "t-b": "blocked" };

    const result = derivePendingGates(messages, taskStates);

    expect(result).toEqual([{ taskId: "t-a", reason: "a 검토 필요", ts: "2026-08-30T00:00:00Z", msgSeq: 1 }]);
  });

  it("returns an empty list for empty messages", () => {
    expect(derivePendingGates([], {})).toEqual([]);
  });

  it("includes a gate missing task_id as taskId '' regardless of taskStates", () => {
    const messages = [makeGate(1, { reason: "사유만 있음" })];

    const result = derivePendingGates(messages, {});

    expect(result).toEqual([{ taskId: "", reason: "사유만 있음", ts: "2026-08-30T00:00:00Z", msgSeq: 1 }]);
  });

  it("ignores a human.gate whose task state changed away from escalated after resolution", () => {
    const messages = [makeGate(1, { task_id: "t-a", reason: "a 검토 필요" })];
    const taskStates: Record<string, TaskStateDto> = { "t-a": "accepted" };

    expect(derivePendingGates(messages, taskStates)).toEqual([]);
  });

  it("sorts results by msgSeq ascending regardless of input order", () => {
    const messages = [
      makeGate(5, { task_id: "t-b", reason: "b" }),
      makeGate(2, { task_id: "t-a", reason: "a" }),
    ];
    const taskStates: Record<string, TaskStateDto> = { "t-a": "escalated", "t-b": "escalated" };

    const result = derivePendingGates(messages, taskStates);

    expect(result.map((g) => g.msgSeq)).toEqual([2, 5]);
  });
});
