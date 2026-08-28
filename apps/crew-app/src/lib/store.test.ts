import { beforeEach, describe, expect, it, vi } from "vitest";

import { createRunStore } from "./store";
import type { RunEventSource } from "./source";
import type { RunEvent } from "./types";

function fakeSource(): RunEventSource {
  return {
    start: vi.fn(async () => {}),
    onEvent: vi.fn(() => () => {}),
    stop: vi.fn(async () => {}),
  };
}

const runStarted: RunEvent = { type: "run_started", run_id: "run_1", goal: "landing page", ts: "t0" };
const specReady: RunEvent = {
  type: "spec_ready",
  spec: { goal: "landing page", non_goals: [], constraints: [], requirements: [], acceptance: [] },
  dag: { tasks: [] },
  sprint: ["t-pm"],
  ts: "t1",
};

function message(seq: number, kind: string = "task.assign"): RunEvent {
  return {
    type: "message",
    seq,
    envelope: {
      id: `env_${seq}`,
      ts: "t",
      sprint: "sprint-1",
      thread: "t-pm",
      from: "lead",
      to: ["agent:pm"],
      kind: kind as never,
      corr: "t-pm",
      body: {},
      artifacts: [],
      requires_ack: false,
      deadline_ms: 1000,
    },
  };
}

describe("RunState.applyEvent — normal accumulation", () => {
  let store: ReturnType<typeof createRunStore>;

  beforeEach(() => {
    store = createRunStore(fakeSource());
  });

  it("accumulates messages and updates spec/dag/sprint/taskStates through a run", () => {
    store.getState().applyEvent(runStarted);
    store.getState().applyEvent(specReady);
    store.getState().applyEvent(message(1));
    store.getState().applyEvent({ type: "task_state_changed", task_id: "t-pm", state: "assigned", ts: "t2" });

    const s = store.getState();
    expect(s.runId).toBe("run_1");
    expect(s.goal).toBe("landing page");
    expect(s.sprint).toEqual(["t-pm"]);
    expect(s.messages).toHaveLength(1);
    expect(s.messages[0].seq).toBe(1);
    expect(s.taskStates["t-pm"]).toBe("assigned");
  });

  it("sets finished on run_finished", () => {
    store.getState().applyEvent({ type: "run_finished", outcome: "completed", ts: "t9" });
    expect(store.getState().finished).toBe("completed");
  });
});

describe("RunState.applyEvent — boundary: empty snapshot", () => {
  it("starts with a fully empty snapshot before any event is applied", () => {
    const store = createRunStore(fakeSource());
    const s = store.getState();
    expect(s.runId).toBeNull();
    expect(s.goal).toBeNull();
    expect(s.spec).toBeNull();
    expect(s.dag).toBeNull();
    expect(s.sprint).toEqual([]);
    expect(s.messages).toEqual([]);
    expect(s.taskStates).toEqual({});
    expect(s.finished).toBeNull();
  });
});

describe("RunState.applyEvent — error/boundary: duplicate seq", () => {
  it("ignores a message whose seq was already applied (idempotent)", () => {
    const store = createRunStore(fakeSource());
    store.getState().applyEvent(message(1));
    store.getState().applyEvent(message(1));
    expect(store.getState().messages).toHaveLength(1);
  });

  it("resets the seen-seq set on a new run_started so seq 1 can recur", () => {
    const store = createRunStore(fakeSource());
    store.getState().applyEvent(message(1));
    store.getState().applyEvent(runStarted);
    store.getState().applyEvent(message(1));
    expect(store.getState().messages).toHaveLength(1);
  });
});

describe("RunState.applyEvent — error/boundary: unknown event type", () => {
  it("warns and leaves state unchanged for an unrecognized type", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const store = createRunStore(fakeSource());
    const before = store.getState();

    store.getState().applyEvent({ type: "future_event", foo: "bar" } as unknown as RunEvent);

    expect(warn).toHaveBeenCalled();
    expect(store.getState()).toEqual(before);
    warn.mockRestore();
  });
});

describe("RunState.startRun", () => {
  it("delegates to the injected RunEventSource", async () => {
    const source = fakeSource();
    const store = createRunStore(source);
    await store.getState().startRun("새 랜딩 페이지");
    expect(source.start).toHaveBeenCalledWith("새 랜딩 페이지");
  });
});
