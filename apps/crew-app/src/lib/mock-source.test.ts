import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { MockEventSource } from "./mock-source";
import { createRunStore } from "./store";
import type { RunEvent } from "./types";

describe("MockEventSource", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("replays the full M3-shaped scenario (5 roles, one designer rework) in order", async () => {
    const source = new MockEventSource(0);
    const received: RunEvent[] = [];
    source.onEvent((ev) => received.push(ev));

    await source.start("간단한 랜딩 페이지");
    await vi.runAllTimersAsync();

    expect(received[0]).toMatchObject({ type: "run_started", goal: "간단한 랜딩 페이지" });
    expect(received[1]).toMatchObject({ type: "spec_ready" });
    expect(received.at(-1)).toMatchObject({ type: "run_finished", outcome: "completed" });

    const specReady = received[1] as Extract<RunEvent, { type: "spec_ready" }>;
    expect(specReady.spec.requirements.map((r) => r.id)).toEqual([
      "REQ-1",
      "REQ-2",
      "REQ-3",
      "REQ-4",
      "REQ-5",
    ]);
    expect(specReady.dag.tasks.map((t) => t.id)).toEqual([
      "t-pm",
      "t-design",
      "t-publish",
      "t-dev",
      "t-qa",
    ]);
    expect(specReady.sprint).toEqual(["t-pm", "t-design", "t-publish", "t-dev", "t-qa"]);

    const messages = received.filter((ev): ev is Extract<RunEvent, { type: "message" }> => ev.type === "message");
    // 4 normal tasks * (assign+ack+result) + designer * (assign+ack+result+change_request+result)
    expect(messages).toHaveLength(4 * 3 + 5);
    expect(messages.map((m) => m.seq)).toEqual(messages.map((_, i) => i + 1));

    const designerKinds = messages
      .filter((m) => m.envelope.thread === "t-design")
      .map((m) => m.envelope.kind);
    expect(designerKinds).toEqual(["task.assign", "task.ack", "task.result", "change_request", "task.result"]);

    const taskStateChanges = received.filter(
      (ev): ev is Extract<RunEvent, { type: "task_state_changed" }> => ev.type === "task_state_changed",
    );
    expect(taskStateChanges).toHaveLength(10);
    expect(taskStateChanges.filter((c) => c.state === "accepted")).toHaveLength(5);
  });

  it("stops delivering events after stop() is called", async () => {
    const source = new MockEventSource(0);
    const received: RunEvent[] = [];
    source.onEvent((ev) => received.push(ev));

    await source.start("goal");
    await source.stop();
    await vi.runAllTimersAsync();

    expect(received).toHaveLength(0);
  });

  it("unsubscribe stops a specific listener from receiving further events", async () => {
    const source = new MockEventSource(0);
    const received: RunEvent[] = [];
    const unsubscribe = source.onEvent((ev) => received.push(ev));
    unsubscribe();

    await source.start("goal");
    await vi.runAllTimersAsync();

    expect(received).toHaveLength(0);
  });
});

describe("MockEventSource + RunState integration", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("drives the store to a fully accepted, completed final state", async () => {
    const source = new MockEventSource(0);
    const store = createRunStore(source);
    source.onEvent(store.getState().applyEvent);

    await store.getState().startRun("간단한 랜딩 페이지");
    await vi.runAllTimersAsync();

    const s = store.getState();
    expect(s.finished).toBe("completed");
    expect(s.dag?.tasks).toHaveLength(5);
    expect(Object.values(s.taskStates)).toEqual(["accepted", "accepted", "accepted", "accepted", "accepted"]);
    expect(s.messages).toHaveLength(4 * 3 + 5);
    // seq dedup holds across the whole replayed scenario too.
    const seqs = s.messages.map((m) => m.seq);
    expect(new Set(seqs).size).toBe(seqs.length);
  });
});
