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
    expect(s.sprintIndex).toBeNull();
    expect(s.sprintSummaries).toEqual([]);
    expect(s.roster).toEqual([]);
    expect(s.sprintWindows).toEqual([]);
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

describe("RunState.applyEvent — sprint/roster events (plan D2)", () => {
  it("sets sprintIndex on sprint_started and appends to sprintSummaries on sprint_finished", () => {
    const store = createRunStore(fakeSource());
    store.getState().applyEvent({ type: "sprint_started", index: 1, task_ids: ["t-pm"], ts: "t1" });
    expect(store.getState().sprintIndex).toBe(1);

    store.getState().applyEvent({ type: "sprint_finished", index: 1, summary: "스프린트 1 완료", ts: "t2" });
    expect(store.getState().sprintSummaries).toEqual([{ index: 1, summary: "스프린트 1 완료" }]);

    store.getState().applyEvent({ type: "sprint_started", index: 2, task_ids: ["t-design"], ts: "t3" });
    expect(store.getState().sprintIndex).toBe(2);
  });

  it("replaces the roster wholesale on roster_changed, never merging with the previous value", () => {
    const store = createRunStore(fakeSource());
    store.getState().applyEvent({
      type: "roster_changed",
      agents: [{ id: "agent:pm", role: "pm", harness: "claude-code", model: "claude-sonnet-5" }],
      ts: "t1",
    });
    expect(store.getState().roster).toEqual([
      { id: "agent:pm", role: "pm", harness: "claude-code", model: "claude-sonnet-5" },
    ]);

    store.getState().applyEvent({
      type: "roster_changed",
      agents: [{ id: "agent:designer", role: "designer", harness: "opencode", model: "claude-sonnet-5" }],
      ts: "t2",
    });
    // Full replace: the pm slot from the first event must be gone, not merged.
    expect(store.getState().roster).toEqual([
      { id: "agent:designer", role: "designer", harness: "opencode", model: "claude-sonnet-5" },
    ]);
  });

  it("ignores a re-applied sprint_finished for the same index (idempotent)", () => {
    const store = createRunStore(fakeSource());
    store.getState().applyEvent({ type: "sprint_finished", index: 1, summary: "첫 요약", ts: "t1" });
    store.getState().applyEvent({ type: "sprint_finished", index: 1, summary: "첫 요약", ts: "t2" });
    expect(store.getState().sprintSummaries).toHaveLength(1);
  });

  it("resets sprintIndex/sprintSummaries/roster on a new run_started", () => {
    const store = createRunStore(fakeSource());
    store.getState().applyEvent({ type: "sprint_started", index: 1, task_ids: [], ts: "t1" });
    store.getState().applyEvent({ type: "sprint_finished", index: 1, summary: "s", ts: "t2" });
    store.getState().applyEvent({
      type: "roster_changed",
      agents: [{ id: "agent:pm", role: "pm", harness: "claude-code", model: "claude-sonnet-5" }],
      ts: "t3",
    });

    store.getState().applyEvent(runStarted);

    const s = store.getState();
    expect(s.sprintIndex).toBeNull();
    expect(s.sprintSummaries).toEqual([]);
    expect(s.roster).toEqual([]);
  });
});

describe("RunState.applyEvent — sprintWindows (plan D3/D4)", () => {
  it("creates a window with endTs=null on sprint_started", () => {
    const store = createRunStore(fakeSource());
    store.getState().applyEvent({ type: "sprint_started", index: 1, task_ids: ["t-pm"], ts: "t1" });
    expect(store.getState().sprintWindows).toEqual([{ index: 1, startTs: "t1", endTs: null }]);
  });

  it("sets endTs on sprint_finished for an existing window", () => {
    const store = createRunStore(fakeSource());
    store.getState().applyEvent({ type: "sprint_started", index: 1, task_ids: ["t-pm"], ts: "t1" });
    store.getState().applyEvent({ type: "sprint_finished", index: 1, summary: "완료", ts: "t2" });
    expect(store.getState().sprintWindows).toEqual([{ index: 1, startTs: "t1", endTs: "t2" }]);
  });

  it("is idempotent: re-applying the same sprint_started/sprint_finished pair yields the same state", () => {
    const store = createRunStore(fakeSource());
    const started: RunEvent = { type: "sprint_started", index: 1, task_ids: ["t-pm"], ts: "t1" };
    const finished: RunEvent = { type: "sprint_finished", index: 1, summary: "완료", ts: "t2" };

    store.getState().applyEvent(started);
    store.getState().applyEvent(finished);
    const after1 = store.getState().sprintWindows;

    store.getState().applyEvent(started);
    store.getState().applyEvent(finished);
    const after2 = store.getState().sprintWindows;

    expect(after2).toEqual(after1);
    expect(after2).toEqual([{ index: 1, startTs: "t1", endTs: "t2" }]);
  });

  it("boundary: sprint_finished with no prior sprint_started creates a window with startTs=endTs", () => {
    const store = createRunStore(fakeSource());
    store.getState().applyEvent({ type: "sprint_finished", index: 3, summary: "요약", ts: "t5" });
    expect(store.getState().sprintWindows).toEqual([{ index: 3, startTs: "t5", endTs: "t5" }]);
  });

  it("resets sprintWindows on a new run_started", () => {
    const store = createRunStore(fakeSource());
    store.getState().applyEvent({ type: "sprint_started", index: 1, task_ids: [], ts: "t1" });
    store.getState().applyEvent(runStarted);
    expect(store.getState().sprintWindows).toEqual([]);
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
