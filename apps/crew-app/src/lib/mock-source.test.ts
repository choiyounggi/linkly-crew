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

  it("replays the full 3-sprint scenario (5 roles, one designer rework, one harness swap) in order", async () => {
    const source = new MockEventSource(0);
    const received: RunEvent[] = [];
    source.onEvent((ev) => received.push(ev));

    await source.start("간단한 랜딩 페이지");
    await vi.runAllTimersAsync();

    expect(received[0]).toMatchObject({ type: "run_started", goal: "간단한 랜딩 페이지" });
    expect(received[1]).toMatchObject({ type: "roster_changed" });
    expect(received[2]).toMatchObject({ type: "spec_ready" });
    expect(received[3]).toMatchObject({ type: "sprint_started", index: 1, task_ids: ["t-pm", "t-design"] });
    expect(received.at(-1)).toMatchObject({ type: "run_finished", outcome: "completed" });

    const specReady = received[2] as Extract<RunEvent, { type: "spec_ready" }>;
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

    const initialRoster = received[1] as Extract<RunEvent, { type: "roster_changed" }>;
    expect(initialRoster.agents).toHaveLength(6);
    expect(initialRoster.agents.every((a) => a.harness === "claude-code")).toBe(true);

    const sprintStarts = received.filter(
      (ev): ev is Extract<RunEvent, { type: "sprint_started" }> => ev.type === "sprint_started",
    );
    expect(sprintStarts.map((s) => s.index)).toEqual([1, 2, 3]);
    expect(sprintStarts.map((s) => s.task_ids)).toEqual([["t-pm", "t-design"], ["t-publish", "t-dev"], ["t-qa"]]);

    const sprintFinishes = received.filter(
      (ev): ev is Extract<RunEvent, { type: "sprint_finished" }> => ev.type === "sprint_finished",
    );
    expect(sprintFinishes.map((s) => s.index)).toEqual([1, 2, 3]);

    const rosterChanges = received.filter(
      (ev): ev is Extract<RunEvent, { type: "roster_changed" }> => ev.type === "roster_changed",
    );
    expect(rosterChanges).toHaveLength(2);
    const swappedRoster = rosterChanges[1];
    const designer = swappedRoster.agents.find((a) => a.role === "designer");
    expect(designer?.harness).toBe("opencode");

    const messages = received.filter((ev): ev is Extract<RunEvent, { type: "message" }> => ev.type === "message");
    // 4 normal tasks * (assign+ack+result) + designer rework(5) + 1 handoff message
    expect(messages).toHaveLength(4 * 3 + 5 + 1);
    expect(messages.map((m) => m.seq)).toEqual(messages.map((_, i) => i + 1));

    const handoffMessages = messages.filter((m) => m.envelope.kind === "handoff");
    expect(handoffMessages).toHaveLength(1);
    expect(handoffMessages[0].envelope.to).toEqual(["agent:designer"]);

    // The swap's handoff message also threads on "t-design" (it targets the
    // designer agent), so it trails the rework round trip in this filter.
    const designerKinds = messages
      .filter((m) => m.envelope.thread === "t-design")
      .map((m) => m.envelope.kind);
    expect(designerKinds).toEqual([
      "task.assign",
      "task.ack",
      "task.result",
      "change_request",
      "task.result",
      "handoff",
    ]);

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

  it("stamps ts at delivery time for every variant, so it progresses (non-decreasing) across the replay instead of freezing (r1 F1)", async () => {
    const source = new MockEventSource(100);
    const received: RunEvent[] = [];
    source.onEvent((ev) => received.push(ev));

    await source.start("간단한 랜딩 페이지");
    await vi.runAllTimersAsync();

    const runStarted = received[0] as Extract<RunEvent, { type: "run_started" }>;
    const specReady = received[2] as Extract<RunEvent, { type: "spec_ready" }>;
    expect(specReady.ts).not.toBe(runStarted.ts);
    expect(new Date(specReady.ts).getTime()).toBeGreaterThan(new Date(runStarted.ts).getTime());

    const topLevelTimes = received
      .map((ev) => ("ts" in ev ? new Date(ev.ts).getTime() : null))
      .filter((t): t is number => t !== null);
    for (let i = 1; i < topLevelTimes.length; i++) {
      expect(topLevelTimes[i]).toBeGreaterThanOrEqual(topLevelTimes[i - 1]);
    }

    const messages = received.filter((ev): ev is Extract<RunEvent, { type: "message" }> => ev.type === "message");
    const firstMessageTs = messages[0].envelope.ts;
    const lastMessageTs = messages.at(-1)!.envelope.ts;
    expect(lastMessageTs).not.toBe(firstMessageTs);
    expect(new Date(lastMessageTs).getTime()).toBeGreaterThan(new Date(firstMessageTs).getTime());
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

describe("MockEventSource — roster/harness methods (plan D5)", () => {
  it("swapHarness updates the roster and replays a handoff message + roster_changed immediately", async () => {
    const source = new MockEventSource();
    const received: RunEvent[] = [];
    source.onEvent((ev) => received.push(ev));

    await source.swapHarness("agent:publisher", "opencode");

    expect(received.map((e) => e.type)).toEqual(["message", "roster_changed"]);
    const [handoffEvent, rosterEvent] = received as [
      Extract<RunEvent, { type: "message" }>,
      Extract<RunEvent, { type: "roster_changed" }>,
    ];
    expect(handoffEvent.envelope.kind).toBe("handoff");
    expect(handoffEvent.envelope.to).toEqual(["agent:publisher"]);
    const publisher = rosterEvent.agents.find((a) => a.id === "agent:publisher");
    expect(publisher?.harness).toBe("opencode");

    const roster = await source.getRoster();
    expect(roster.agents.find((a) => a.id === "agent:publisher")?.harness).toBe("opencode");
  });

  it("swapHarness rejects for an unknown agent id and leaves the roster unchanged", async () => {
    const source = new MockEventSource();
    const before = await source.getRoster();

    await expect(source.swapHarness("agent:nope", "opencode")).rejects.toThrow(/unknown agent id/);

    const after = await source.getRoster();
    expect(after).toEqual(before);
  });

  it("getRoster/setRoster round-trip a full Roster (with instructions) in memory", async () => {
    const source = new MockEventSource();
    const initial = await source.getRoster();
    expect(initial.agents).toHaveLength(6);

    const updated: typeof initial = {
      agents: initial.agents.map((a) => (a.role === "lead" ? { ...a, model: "claude-opus-5" } : a)),
    };
    await source.setRoster(updated);

    const after = await source.getRoster();
    expect(after.agents.find((a) => a.role === "lead")?.model).toBe("claude-opus-5");
  });

  it("listPresets returns the 3 built-in presets with the contract's fixed placements", async () => {
    const source = new MockEventSource();
    const presets = await source.listPresets();

    expect(presets.map((p) => p.name)).toEqual(["클로드 5인팀", "절약 모드", "혼합 실험"]);
    for (const preset of presets) {
      expect(preset.roster.agents).toHaveLength(6);
    }
    const mixed = presets.find((p) => p.name === "혼합 실험")!;
    expect(mixed.roster.agents.find((a) => a.role === "publisher")?.harness).toBe("opencode");
  });

  it("detectHarnesses reports installed/adapter status for every known harness, including unsupported ones", async () => {
    const source = new MockEventSource();
    const detected = await source.detectHarnesses();

    expect(detected.map((h) => h.id).sort()).toEqual(
      ["claude-code", "codex", "gemini", "grok", "ollama", "opencode"].sort(),
    );
    const claudeCode = detected.find((h) => h.id === "claude-code")!;
    expect(claudeCode).toMatchObject({ installed: true, adapter: "real" });
    const unsupported = detected.find((h) => h.id === "grok")!;
    expect(unsupported).toMatchObject({ installed: false, adapter: "none", path: null });
  });
});

describe("MockEventSource + RunState integration", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("drives the store to a fully accepted, completed final state across 3 sprints with the designer swap applied", async () => {
    const source = new MockEventSource(0);
    const store = createRunStore(source);
    source.onEvent(store.getState().applyEvent);

    await store.getState().startRun("간단한 랜딩 페이지");
    await vi.runAllTimersAsync();

    const s = store.getState();
    expect(s.finished).toBe("completed");
    expect(s.dag?.tasks).toHaveLength(5);
    expect(Object.values(s.taskStates)).toEqual(["accepted", "accepted", "accepted", "accepted", "accepted"]);
    expect(s.messages).toHaveLength(4 * 3 + 5 + 1);
    // seq dedup holds across the whole replayed scenario too.
    const seqs = s.messages.map((m) => m.seq);
    expect(new Set(seqs).size).toBe(seqs.length);

    expect(s.sprintIndex).toBe(3);
    expect(s.sprintSummaries.map((sum) => sum.index)).toEqual([1, 2, 3]);
    expect(s.roster).toHaveLength(6);
    expect(s.roster.find((a) => a.role === "designer")?.harness).toBe("opencode");
  });
});
