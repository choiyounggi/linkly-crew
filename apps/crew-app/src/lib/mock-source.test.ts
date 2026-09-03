import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { MockEventSource } from "./mock-source";
import { createRunStore } from "./store";
import type { RunEvent } from "./types";

const MOCK_RUN_ID = "run_mock";

describe("MockEventSource", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("returns the fixed demo run id from start()", async () => {
    const source = new MockEventSource(0);
    await expect(source.start("goal", false, null)).resolves.toBe(MOCK_RUN_ID);
  });

  it("replays the full 3-sprint scenario (5 roles, one designer rework, one harness swap) in order, all tagged with the fixed demo run id", async () => {
    const source = new MockEventSource(0);
    const received: { runId: string; ev: RunEvent }[] = [];
    source.onEvent((runId, ev) => received.push({ runId, ev }));

    await source.start("간단한 랜딩 페이지", false, null);
    await vi.runAllTimersAsync();

    expect(received.every((r) => r.runId === MOCK_RUN_ID)).toBe(true);
    const events = received.map((r) => r.ev);

    expect(events[0]).toMatchObject({ type: "run_started", goal: "간단한 랜딩 페이지" });
    expect(events[1]).toMatchObject({ type: "roster_changed" });
    expect(events[2]).toMatchObject({ type: "spec_ready" });
    expect(events[3]).toMatchObject({ type: "sprint_started", index: 1, task_ids: ["t-pm", "t-design"] });
    expect(events.at(-1)).toMatchObject({ type: "run_finished", outcome: "completed" });

    const specReady = events[2] as Extract<RunEvent, { type: "spec_ready" }>;
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

    const initialRoster = events[1] as Extract<RunEvent, { type: "roster_changed" }>;
    expect(initialRoster.agents).toHaveLength(6);
    expect(initialRoster.agents.every((a) => a.harness === "claude-code")).toBe(true);

    const sprintStarts = events.filter(
      (ev): ev is Extract<RunEvent, { type: "sprint_started" }> => ev.type === "sprint_started",
    );
    expect(sprintStarts.map((s) => s.index)).toEqual([1, 2, 3]);
    expect(sprintStarts.map((s) => s.task_ids)).toEqual([["t-pm", "t-design"], ["t-publish", "t-dev"], ["t-qa"]]);

    const sprintFinishes = events.filter(
      (ev): ev is Extract<RunEvent, { type: "sprint_finished" }> => ev.type === "sprint_finished",
    );
    expect(sprintFinishes.map((s) => s.index)).toEqual([1, 2, 3]);

    const rosterChanges = events.filter(
      (ev): ev is Extract<RunEvent, { type: "roster_changed" }> => ev.type === "roster_changed",
    );
    expect(rosterChanges).toHaveLength(2);
    const swappedRoster = rosterChanges[1];
    const designer = swappedRoster.agents.find((a) => a.role === "designer");
    expect(designer?.harness).toBe("opencode");

    const messages = events.filter((ev): ev is Extract<RunEvent, { type: "message" }> => ev.type === "message");
    // 3 normal tasks * (assign+ack+result) + qa escalation(assign+ack+blocked+human.gate)
    // + designer rework(5) + 1 handoff message
    expect(messages).toHaveLength(3 * 3 + 4 + 5 + 1);
    expect(messages.map((m) => m.seq)).toEqual(messages.map((_, i) => i + 1));

    const handoffMessages = messages.filter((m) => m.envelope.kind === "handoff");
    expect(handoffMessages).toHaveLength(1);
    expect(handoffMessages[0].envelope.to).toEqual(["agent:designer"]);

    const taskStateChanges = events.filter(
      (ev): ev is Extract<RunEvent, { type: "task_state_changed" }> => ev.type === "task_state_changed",
    );
    expect(taskStateChanges).toHaveLength(4 * 2 + 3);
    expect(taskStateChanges.filter((c) => c.state === "accepted")).toHaveLength(4);
    expect(taskStateChanges.filter((c) => c.task_id === "t-qa").map((c) => c.state)).toEqual([
      "assigned",
      "blocked",
      "escalated",
    ]);
  });

  it("escalates t-qa via a blocked message + human.gate carrying body.task_id, reaching state escalated (contracts-m7.md §E8)", async () => {
    const source = new MockEventSource(0);
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    await source.start("간단한 랜딩 페이지", false, null);
    await vi.runAllTimersAsync();

    const messages = received.filter((ev): ev is Extract<RunEvent, { type: "message" }> => ev.type === "message");
    const qaMessages = messages.filter((m) => m.envelope.thread === "t-qa");
    expect(qaMessages.map((m) => m.envelope.kind)).toEqual(["task.assign", "task.ack", "blocked", "human.gate"]);

    const gate = qaMessages.find((m) => m.envelope.kind === "human.gate")!;
    expect(gate.envelope.body).toMatchObject({ task_id: "t-qa" });

    const finalQaState = received
      .filter((ev): ev is Extract<RunEvent, { type: "task_state_changed" }> => ev.type === "task_state_changed")
      .filter((c) => c.task_id === "t-qa")
      .at(-1);
    expect(finalQaState?.state).toBe("escalated");
    expect(received.at(-1)).toMatchObject({ type: "run_finished", outcome: "completed" });
  });

  it("stops delivering events after stop(runId) is called", async () => {
    const source = new MockEventSource(0);
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    await source.start("goal", false, null);
    await source.stop(MOCK_RUN_ID);
    await vi.runAllTimersAsync();

    expect(received).toHaveLength(0);
  });

  it("resync() is a no-op that resolves without emitting anything (mock has no separate backend state)", async () => {
    const source = new MockEventSource(0);
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    await expect(source.resync(MOCK_RUN_ID)).resolves.toBeUndefined();
    expect(received).toHaveLength(0);
  });

  it("listRuns reflects the started demo run, and an empty array before any start / after remove", async () => {
    const source = new MockEventSource(0);
    await expect(source.listRuns()).resolves.toEqual([]);

    await source.start("goal", false, null);
    await expect(source.listRuns()).resolves.toEqual([{ run_id: MOCK_RUN_ID, goal: "goal", finished: null }]);

    await source.remove(MOCK_RUN_ID);
    await expect(source.listRuns()).resolves.toEqual([]);
  });

  it("stamps ts at delivery time for every variant, so it progresses (non-decreasing) across the replay instead of freezing (r1 F1)", async () => {
    const source = new MockEventSource(100);
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    await source.start("간단한 랜딩 페이지", false, null);
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
  });

  it("unsubscribe stops a specific listener from receiving further events", async () => {
    const source = new MockEventSource(0);
    const received: RunEvent[] = [];
    const unsubscribe = source.onEvent((_runId, ev) => received.push(ev));
    unsubscribe();

    await source.start("goal", false, null);
    await vi.runAllTimersAsync();

    expect(received).toHaveLength(0);
  });
});

describe("MockEventSource — roster/harness methods (plan D5, unchanged — no runId)", () => {
  it("swapHarness (D8: runId first, unused by this single-run mock) updates the roster and replays a handoff + roster_changed immediately", async () => {
    const source = new MockEventSource();
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    await source.swapHarness(MOCK_RUN_ID, "agent:publisher", "opencode");

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

    await expect(source.swapHarness(MOCK_RUN_ID, "agent:nope", "opencode")).rejects.toThrow(/unknown agent id/);

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

describe("MockEventSource — gate/search methods (plan D4, D8: runId first)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("resolveGate('approve') replays a human.response message then assigned->accepted", async () => {
    const source = new MockEventSource();
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    await source.resolveGate(MOCK_RUN_ID, "t-qa", "approve", "재현 확인, 승인함");

    expect(received.map((e) => e.type)).toEqual(["message", "task_state_changed", "task_state_changed"]);
    const [responseEvent, assignedEvent, acceptedEvent] = received as [
      Extract<RunEvent, { type: "message" }>,
      Extract<RunEvent, { type: "task_state_changed" }>,
      Extract<RunEvent, { type: "task_state_changed" }>,
    ];
    expect(responseEvent.envelope.kind).toBe("human.response");
    expect(responseEvent.envelope.body).toMatchObject({ task_id: "t-qa", decision: "approve" });
    expect(assignedEvent).toMatchObject({ task_id: "t-qa", state: "assigned" });
    expect(acceptedEvent).toMatchObject({ task_id: "t-qa", state: "accepted" });
  });

  it("resolveGate('reject') replays a human.response message then blocked", async () => {
    const source = new MockEventSource();
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    await source.resolveGate(MOCK_RUN_ID, "t-qa", "reject", "재현 안 됨, 반려");

    expect(received.map((e) => e.type)).toEqual(["message", "task_state_changed"]);
    const stateEvent = received[1] as Extract<RunEvent, { type: "task_state_changed" }>;
    expect(stateEvent).toMatchObject({ task_id: "t-qa", state: "blocked" });
  });

  it("resolveGate rejects for an unknown task id, delivering no events", async () => {
    const source = new MockEventSource();
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    await expect(source.resolveGate(MOCK_RUN_ID, "t-nope", "approve", "")).rejects.toThrow(/unknown task id/);
    expect(received).toHaveLength(0);
  });

  it("searchMessages filters delivered messages by kind/from/body, case-insensitively", async () => {
    const source = new MockEventSource(0);
    await source.start("간단한 랜딩 페이지", false, null);
    await vi.runAllTimersAsync();

    const byKind = await source.searchMessages(MOCK_RUN_ID, "HANDOFF");
    expect(byKind).toHaveLength(1);
    expect(byKind[0].envelope.kind).toBe("handoff");

    const byBody = await source.searchMessages(MOCK_RUN_ID, "req-4");
    expect(byBody.length).toBeGreaterThan(0);
    expect(byBody.every((m) => JSON.stringify(m.envelope.body).toLowerCase().includes("req-4"))).toBe(true);
  });

  it("searchMessages returns an empty array for a query with no matches and for an empty delivered log", async () => {
    const emptySource = new MockEventSource();
    expect(await emptySource.searchMessages(MOCK_RUN_ID, "anything")).toEqual([]);

    const source = new MockEventSource(0);
    await source.start("간단한 랜딩 페이지", false, null);
    await vi.runAllTimersAsync();
    expect(await source.searchMessages(MOCK_RUN_ID, "no-such-token-xyz")).toEqual([]);
  });
});

describe("MockEventSource.createProject", () => {
  it("resolves with a fake ProjectInfo for a valid name", async () => {
    const source = new MockEventSource();
    await expect(source.createProject("my-app")).resolves.toEqual({ name: "my-app", path: "/mock/my-app" });
  });

  it("rejects with invalid_name for an empty or malformed name (boundary/error, mirrors src-tauri validate_name)", async () => {
    const source = new MockEventSource();
    await expect(source.createProject("")).rejects.toThrow("invalid_name");
    await expect(source.createProject("My App")).rejects.toThrow("invalid_name");
    await expect(source.createProject("-leading-hyphen")).rejects.toThrow("invalid_name");
  });
});

describe("MockEventSource.listProjects", () => {
  it("resolves a name-ascending list, each item with name and path", async () => {
    const source = new MockEventSource();
    const projects = await source.listProjects!();
    expect(projects.map((p) => p.name)).toEqual(["alpha", "beta"]);
    for (const p of projects) {
      expect(p).toEqual(expect.objectContaining({ name: expect.any(String), path: expect.any(String) }));
    }
  });

  it("is deterministic across repeated calls (boundary — guards against non-deterministic/random data)", async () => {
    const source = new MockEventSource();
    const first = await source.listProjects!();
    const second = await source.listProjects!();
    expect(second).toEqual(first);
  });

  // listProjects cannot error by construction (no invoke, no filesystem access) — no error case.

  it("does not change start()'s scenario replay when a projectRoot is passed (boundary — third arg is a no-op here)", async () => {
    vi.useFakeTimers();
    const source = new MockEventSource(0);
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    const runId = await source.start("goal", false, "/mock/demo");
    await vi.runAllTimersAsync();

    expect(runId).toBe(MOCK_RUN_ID);
    expect(received[0]).toMatchObject({ type: "run_started", goal: "goal" });
    vi.useRealTimers();
  });
});

describe("MockEventSource + RunState integration", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("drives the store to a completed final state across 3 sprints, with t-qa escalated and the designer swap applied", async () => {
    const source = new MockEventSource(0);
    const store = createRunStore(source);
    source.onEvent(store.getState().applyEvent);

    const runId = await store.getState().startChannel("간단한 랜딩 페이지", false, null);
    await vi.runAllTimersAsync();

    const s = store.getState().channels[runId];
    expect(s.finished).toBe("completed");
    expect(s.dag?.tasks).toHaveLength(5);
    expect(Object.values(s.taskStates)).toEqual(["accepted", "accepted", "accepted", "accepted", "escalated"]);
    expect(s.messages).toHaveLength(3 * 3 + 4 + 5 + 1);
    const seqs = s.messages.map((m) => m.seq);
    expect(new Set(seqs).size).toBe(seqs.length);

    expect(s.sprintIndex).toBe(3);
    expect(s.sprintSummaries.map((sum) => sum.index)).toEqual([1, 2, 3]);
    expect(s.roster).toHaveLength(6);
    expect(s.roster.find((a) => a.role === "designer")?.harness).toBe("opencode");
  });
});
