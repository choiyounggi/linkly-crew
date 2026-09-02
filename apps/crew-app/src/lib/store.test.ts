import { beforeEach, describe, expect, it, vi } from "vitest";

import { createRunStore } from "./store";
import type { RunEventSource } from "./source";
import type { RunEvent } from "./types";

function fakeSource(overrides: Partial<RunEventSource> = {}): RunEventSource {
  return {
    start: vi.fn(async () => "run_1"),
    onEvent: vi.fn(() => () => {}),
    stop: vi.fn(async () => {}),
    remove: vi.fn(async () => {}),
    resync: vi.fn(async () => {}),
    listRuns: vi.fn(async () => []),
    createProject: vi.fn(async (name: string) => ({ name, path: `/tmp/${name}` })),
    ...overrides,
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

describe("RunState.startChannel — creates the channel before delivering any event (R4/D3)", () => {
  it("inserts the channel, selects it, and only then calls resync (so snapshot/live events for it are never dropped)", async () => {
    const order: string[] = [];
    const source = fakeSource({
      start: vi.fn(async () => {
        order.push("start");
        return "run_1";
      }),
      resync: vi.fn(async (runId: string) => {
        order.push(`resync:${runId}`);
      }),
    });
    const store = createRunStore(source);

    const runId = await store.getState().startChannel("landing page", false);

    expect(runId).toBe("run_1");
    expect(order).toEqual(["start", "resync:run_1"]);
    const s = store.getState();
    expect(s.channels["run_1"]).toBeDefined();
    expect(s.channels["run_1"].goal).toBe("landing page");
    expect(s.channels["run_1"].scripted).toBe(false);
    expect(s.channelOrder).toEqual(["run_1"]);
    expect(s.activeRunId).toBe("run_1");
  });

  it("appends a second channel without disturbing the first (multi-channel ordering)", async () => {
    const source = fakeSource({
      start: vi
        .fn()
        .mockResolvedValueOnce("run_1")
        .mockResolvedValueOnce("run_2"),
    });
    const store = createRunStore(source);

    await store.getState().startChannel("first", false);
    await store.getState().startChannel("second", true);

    const s = store.getState();
    expect(s.channelOrder).toEqual(["run_1", "run_2"]);
    expect(s.channels["run_1"].goal).toBe("first");
    expect(s.channels["run_2"].goal).toBe("second");
    expect(s.channels["run_2"].scripted).toBe(true);
    expect(s.activeRunId).toBe("run_2");
  });
});

// 교차 run_id 오염 없음 (R4): 두 채널이 동시에 진행 중이어도 이벤트가 서로 오염되지 않는다.
describe("RunState.applyEvent — cross run_id isolation (normal, R4)", () => {
  it("routes each event only to its own channel — no contamination between two concurrently active runs", async () => {
    const source = fakeSource({
      start: vi
        .fn()
        .mockResolvedValueOnce("run_a")
        .mockResolvedValueOnce("run_b"),
    });
    const store = createRunStore(source);

    await store.getState().startChannel("goal a", false);
    await store.getState().startChannel("goal b", false);

    store.getState().applyEvent("run_a", message(1, "task.assign"));
    store.getState().applyEvent("run_b", message(1, "task.result"));
    store.getState().applyEvent("run_a", { type: "task_state_changed", task_id: "t-pm", state: "assigned", ts: "t" });

    const s = store.getState();
    expect(s.channels["run_a"].messages).toHaveLength(1);
    expect(s.channels["run_a"].messages[0].envelope.kind).toBe("task.assign");
    expect(s.channels["run_b"].messages).toHaveLength(1);
    expect(s.channels["run_b"].messages[0].envelope.kind).toBe("task.result");
    expect(s.channels["run_a"].taskStates["t-pm"]).toBe("assigned");
    expect(s.channels["run_b"].taskStates).toEqual({});
  });

  it("keeps each channel's seq-dedup independent — the same seq number in two channels is not treated as a duplicate across them", async () => {
    const source = fakeSource({
      start: vi
        .fn()
        .mockResolvedValueOnce("run_a")
        .mockResolvedValueOnce("run_b"),
    });
    const store = createRunStore(source);
    await store.getState().startChannel("goal a", false);
    await store.getState().startChannel("goal b", false);

    store.getState().applyEvent("run_a", message(1));
    store.getState().applyEvent("run_b", message(1));

    expect(store.getState().channels["run_a"].messages).toHaveLength(1);
    expect(store.getState().channels["run_b"].messages).toHaveLength(1);
  });
});

describe("RunState.applyEvent — unknown run_id is ignored, not auto-created (boundary, D3)", () => {
  it("warns and does not create a channel for an event whose run_id was never started", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const store = createRunStore(fakeSource());

    store.getState().applyEvent("run_ghost", runStarted);

    expect(warn).toHaveBeenCalled();
    expect(store.getState().channels).toEqual({});
    expect(store.getState().channelOrder).toEqual([]);
    warn.mockRestore();
  });
});

describe("RunState.applyEvent — accumulation through a channel's lifecycle", () => {
  it("accumulates messages and updates spec/dag/sprint/taskStates for the right channel", async () => {
    const store = createRunStore(fakeSource());
    await store.getState().startChannel("landing page", false);

    store.getState().applyEvent("run_1", specReady);
    store.getState().applyEvent("run_1", message(1));
    store.getState().applyEvent("run_1", { type: "task_state_changed", task_id: "t-pm", state: "assigned", ts: "t2" });

    const channel = store.getState().channels["run_1"];
    expect(channel.goal).toBe("landing page");
    expect(channel.sprint).toEqual(["t-pm"]);
    expect(channel.messages).toHaveLength(1);
    expect(channel.messages[0].seq).toBe(1);
    expect(channel.taskStates["t-pm"]).toBe("assigned");
  });

  it("sets finished on run_finished", async () => {
    const store = createRunStore(fakeSource());
    await store.getState().startChannel("goal", false);

    store.getState().applyEvent("run_1", { type: "run_finished", outcome: "completed", ts: "t9" });

    expect(store.getState().channels["run_1"].finished).toBe("completed");
  });

  it("ignores a message whose seq was already applied for that channel (idempotent)", async () => {
    const store = createRunStore(fakeSource());
    await store.getState().startChannel("goal", false);

    store.getState().applyEvent("run_1", message(1));
    store.getState().applyEvent("run_1", message(1));

    expect(store.getState().channels["run_1"].messages).toHaveLength(1);
  });

  it("resets the channel's fields (and its seen-seq set) on a re-applied run_started for that run_id", async () => {
    const store = createRunStore(fakeSource());
    await store.getState().startChannel("goal", false);
    store.getState().applyEvent("run_1", message(1));

    store.getState().applyEvent("run_1", runStarted);
    store.getState().applyEvent("run_1", message(1));

    expect(store.getState().channels["run_1"].messages).toHaveLength(1);
    expect(store.getState().channels["run_1"].goal).toBe("landing page");
  });

  it("replaces the roster wholesale on roster_changed, never merging with the previous value", async () => {
    const store = createRunStore(fakeSource());
    await store.getState().startChannel("goal", false);

    store.getState().applyEvent("run_1", {
      type: "roster_changed",
      agents: [{ id: "agent:pm", role: "pm", harness: "claude-code", model: "claude-sonnet-5" }],
      ts: "t1",
    });
    store.getState().applyEvent("run_1", {
      type: "roster_changed",
      agents: [{ id: "agent:designer", role: "designer", harness: "opencode", model: "claude-sonnet-5" }],
      ts: "t2",
    });

    expect(store.getState().channels["run_1"].roster).toEqual([
      { id: "agent:designer", role: "designer", harness: "opencode", model: "claude-sonnet-5" },
    ]);
  });

  it("warns and leaves the channel unchanged for an unrecognized event type", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const store = createRunStore(fakeSource());
    await store.getState().startChannel("goal", false);
    const before = store.getState().channels["run_1"];

    store.getState().applyEvent("run_1", { type: "future_event", foo: "bar" } as unknown as RunEvent);

    expect(warn).toHaveBeenCalled();
    expect(store.getState().channels["run_1"]).toEqual(before);
    warn.mockRestore();
  });
});

describe("RunState.selectChannel", () => {
  it("sets activeRunId and triggers a resync of that channel (D3)", async () => {
    const resync = vi.fn(async () => {});
    const source = fakeSource({ resync });
    const store = createRunStore(source);
    await store.getState().startChannel("a", false);
    resync.mockClear();

    store.getState().selectChannel("run_1");

    expect(store.getState().activeRunId).toBe("run_1");
    expect(resync).toHaveBeenCalledWith("run_1");
  });

  it("does not throw when resync rejects — the selection still takes effect (error/boundary)", async () => {
    const source = fakeSource({ resync: vi.fn(async () => Promise.reject(new Error("run_not_found"))) });
    const store = createRunStore(source);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

    expect(() => store.getState().selectChannel("run_x")).not.toThrow();
    expect(store.getState().activeRunId).toBe("run_x");

    await Promise.resolve();
    await Promise.resolve();
    warn.mockRestore();
  });
});

describe("RunState.closeChannel — stop+remove (D1), local cleanup regardless of outcome", () => {
  let store: ReturnType<typeof createRunStore>;
  let stop: ReturnType<typeof vi.fn>;
  let remove: ReturnType<typeof vi.fn>;

  beforeEach(async () => {
    stop = vi.fn(async () => {});
    remove = vi.fn(async () => {});
    store = createRunStore(fakeSource({ stop, remove }));
    await store.getState().startChannel("goal", false);
  });

  it("calls stop_run then remove_run and drops the channel locally", async () => {
    await store.getState().closeChannel("run_1");

    expect(stop).toHaveBeenCalledWith("run_1");
    expect(remove).toHaveBeenCalledWith("run_1");
    const s = store.getState();
    expect(s.channels["run_1"]).toBeUndefined();
    expect(s.channelOrder).toEqual([]);
    expect(s.activeRunId).toBeNull();
  });

  it("selects the next remaining channel when the active one is closed", async () => {
    const source = fakeSource({
      start: vi
        .fn()
        .mockResolvedValueOnce("run_1")
        .mockResolvedValueOnce("run_2"),
    });
    const s2 = createRunStore(source);
    await s2.getState().startChannel("a", false);
    await s2.getState().startChannel("b", false);
    expect(s2.getState().activeRunId).toBe("run_2");

    await s2.getState().closeChannel("run_2");

    expect(s2.getState().activeRunId).toBe("run_1");
    expect(s2.getState().channelOrder).toEqual(["run_1"]);
  });

  it("still removes the channel locally even when stop_run/remove_run reject (error/boundary)", async () => {
    stop.mockRejectedValueOnce(new Error("run_not_found"));
    remove.mockRejectedValueOnce(new Error("run_not_found"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

    await store.getState().closeChannel("run_1");

    expect(store.getState().channels["run_1"]).toBeUndefined();
    warn.mockRestore();
  });
});

describe("RunState — empty store (boundary)", () => {
  it("starts with no channels, no active run, empty order", () => {
    const store = createRunStore(fakeSource());
    const s = store.getState();
    expect(s.channels).toEqual({});
    expect(s.activeRunId).toBeNull();
    expect(s.channelOrder).toEqual([]);
  });
});
