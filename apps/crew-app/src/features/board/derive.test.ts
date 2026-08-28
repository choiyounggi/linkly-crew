import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createRunStore } from "../../lib/store";
import { MockEventSource } from "../../lib/mock-source";
import type { Envelope, MessageKind, TaskDag, TaskStateDto } from "../../lib/types";
import { deriveBoard } from "./derive";

const TASK_A = {
  id: "t-a",
  role: "designer" as const,
  title: "task A",
  brief: "brief",
  dod: [],
  deps: [],
  artifacts_expected: [],
};
const TASK_B = {
  id: "t-b",
  role: "developer" as const,
  title: "task B",
  brief: "brief",
  dod: [],
  deps: [],
  artifacts_expected: [],
};

const DAG: TaskDag = { tasks: [TASK_A, TASK_B] };

function env(
  seq: number,
  fields: { kind: MessageKind; corr: string; body?: unknown; thread?: string },
): { seq: number; envelope: Envelope } {
  return {
    seq,
    envelope: {
      id: `env_${seq}`,
      ts: "t",
      sprint: "sprint-1",
      thread: fields.thread ?? fields.corr,
      from: "lead",
      to: ["agent"],
      kind: fields.kind,
      corr: fields.corr,
      body: fields.body ?? {},
      artifacts: [],
      requires_ack: false,
      deadline_ms: 1000,
    },
  };
}

function assignMsg(seq: number, taskId: string, corr = taskId): { seq: number; envelope: Envelope } {
  return env(seq, { kind: "task.assign", corr, body: { task: { id: taskId } } });
}

describe("deriveBoard — boundary: no run / unknown ids", () => {
  it("returns an empty board with no crash when dag is null (pre-run)", () => {
    const board = deriveBoard(null, {}, []);
    expect(board).toEqual({ pending: [], assigned: [], review: [], accepted: [], escalated: [] });
  });

  it("ignores taskStates entries whose id is not in dag.tasks", () => {
    const taskStates: Record<string, TaskStateDto> = { "t-a": "pending", "t-unknown": "accepted" };
    const board = deriveBoard(DAG, taskStates, []);
    expect(board.pending.map((c) => c.task.id)).toEqual(["t-a", "t-b"]);
    expect(board.accepted).toEqual([]);
  });

  it("places an escalated task in the 차단 column regardless of messages", () => {
    const taskStates: Record<string, TaskStateDto> = { "t-a": "escalated", "t-b": "pending" };
    const board = deriveBoard(DAG, taskStates, []);
    expect(board.escalated.map((c) => c.task.id)).toEqual(["t-a"]);
  });
});

describe("deriveBoard — normal: full scenario replay reaches all-accepted", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("puts every task in the 완료 column once the mock scenario finishes", async () => {
    const source = new MockEventSource(0);
    const store = createRunStore(source);
    source.onEvent(store.getState().applyEvent);

    await store.getState().startRun("간단한 랜딩 페이지");
    await vi.runAllTimersAsync();

    const s = store.getState();
    const board = deriveBoard(s.dag, s.taskStates, s.messages);
    expect(board.accepted).toHaveLength(5);
    expect(board.pending).toHaveLength(0);
    expect(board.assigned).toHaveLength(0);
    expect(board.review).toHaveLength(0);
    expect(board.escalated).toHaveLength(0);
  });
});

describe("deriveBoard — intermediate: assigned + task.result -> 검토, change_request -> badge", () => {
  it("puts an assigned task with a task.result into 검토 with rework badge 0", () => {
    const messages = [assignMsg(1, "t-a"), env(2, { kind: "task.result", corr: "t-a" })];
    const board = deriveBoard(DAG, { "t-a": "assigned", "t-b": "pending" }, messages);
    expect(board.review.map((c) => c.task.id)).toEqual(["t-a"]);
    expect(board.review[0].reworkCount).toBe(0);
  });

  it("moves back out of 검토 and sets badge=1 after one change_request following the result", () => {
    const messages = [
      assignMsg(1, "t-a"),
      env(2, { kind: "task.result", corr: "t-a" }),
      env(3, { kind: "change_request", corr: "t-a" }),
    ];
    const board = deriveBoard(DAG, { "t-a": "assigned", "t-b": "pending" }, messages);
    expect(board.review).toEqual([]);
    expect(board.assigned).toHaveLength(1);
    expect(board.assigned[0].task.id).toBe("t-a");
    expect(board.assigned[0].reworkCount).toBe(1);
  });

  it("returns to 검토 with badge=1 once a fresh task.result follows the change_request", () => {
    const messages = [
      assignMsg(1, "t-a"),
      env(2, { kind: "task.result", corr: "t-a" }),
      env(3, { kind: "change_request", corr: "t-a" }),
      env(4, { kind: "task.result", corr: "t-a" }),
    ];
    const board = deriveBoard(DAG, { "t-a": "assigned", "t-b": "pending" }, messages);
    expect(board.review.map((c) => c.task.id)).toEqual(["t-a"]);
    expect(board.review[0].reworkCount).toBe(1);
  });

  it("stays in 진행 (not 검토) for a plain assigned task with no task.result yet", () => {
    const messages = [assignMsg(1, "t-a")];
    const board = deriveBoard(DAG, { "t-a": "assigned" }, messages);
    expect(board.assigned.map((c) => c.task.id)).toEqual(["t-a"]);
    expect(board.review).toEqual([]);
  });

  it("ignores a task.assign whose body doesn't match the {task:{id}} shape (guarded)", () => {
    const messages = [
      env(1, { kind: "task.assign", corr: "t-a", body: { unexpected: true } }),
      env(2, { kind: "task.result", corr: "t-a" }),
    ];
    const board = deriveBoard(DAG, { "t-a": "assigned" }, messages);
    // No matching assign found for t-a -> not in review, rework count 0.
    expect(board.review).toEqual([]);
    expect(board.assigned[0].reworkCount).toBe(0);
  });
});
