import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createRunStore } from "../../lib/store";
import { MockEventSource } from "../../lib/mock-source";
import type { Envelope, MessageKind, RosterAgentDto, TaskDag } from "../../lib/types";
import { AVATAR_INITIALS, avatarInitials, deriveRail } from "./derive";

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
  fields: { kind: MessageKind; corr: string; body?: unknown; to?: string[] },
): { seq: number; envelope: Envelope } {
  return {
    seq,
    envelope: {
      id: `env_${seq}`,
      ts: "t",
      sprint: "sprint-1",
      thread: fields.corr,
      from: "lead",
      to: fields.to ?? ["agent:designer"],
      kind: fields.kind,
      corr: fields.corr,
      body: fields.body ?? {},
      artifacts: [],
      requires_ack: false,
      deadline_ms: 1000,
    },
  };
}

function assignMsg(seq: number, taskId: string, to: string[], corr = taskId): { seq: number; envelope: Envelope } {
  return env(seq, { kind: "task.assign", corr, body: { task: { id: taskId } }, to });
}

describe("deriveRail — normal: assign -> working -> result -> awaiting -> accepted -> idle", () => {
  it("shows working right after task.assign, before any result", () => {
    const messages = [assignMsg(1, "t-a", ["agent:designer"])];
    const cards = deriveRail(DAG, { "t-a": "assigned" }, messages, "run_1", null);
    const designer = cards.find((c) => c.role === "designer");
    expect(designer).toEqual({
      id: "agent:designer",
      role: "designer",
      status: "working",
      currentTaskId: "t-a",
      harness: "claude-code",
    });
  });

  it("shows awaiting once task.result is sent but the task is still 'assigned'", () => {
    const messages = [
      assignMsg(1, "t-a", ["agent:designer"]),
      env(2, { kind: "task.result", corr: "t-a" }),
    ];
    const cards = deriveRail(DAG, { "t-a": "assigned" }, messages, "run_1", null);
    const designer = cards.find((c) => c.role === "designer");
    expect(designer?.status).toBe("awaiting");
    expect(designer?.currentTaskId).toBe("t-a");
  });

  it("shows idle once the task is accepted", () => {
    const messages = [
      assignMsg(1, "t-a", ["agent:designer"]),
      env(2, { kind: "task.result", corr: "t-a" }),
    ];
    const cards = deriveRail(DAG, { "t-a": "accepted" }, messages, "run_1", null);
    const designer = cards.find((c) => c.role === "designer");
    expect(designer?.status).toBe("idle");
    expect(designer?.currentTaskId).toBeNull();
  });

  it("lead is working while the sprint is in flight and idle once finished", () => {
    const inFlight = deriveRail(DAG, {}, [], "run_1", null);
    expect(inFlight.find((c) => c.role === "lead")?.status).toBe("working");

    const finished = deriveRail(DAG, {}, [], "run_1", "completed");
    expect(finished.find((c) => c.role === "lead")?.status).toBe("idle");

    const notStarted = deriveRail(DAG, {}, [], null, null);
    expect(notStarted.find((c) => c.role === "lead")?.status).toBe("idle");
  });
});

describe("deriveRail — rework: change_request sends the agent back to working", () => {
  it("goes back to working after a change_request following task.result", () => {
    const messages = [
      assignMsg(1, "t-a", ["agent:designer"]),
      env(2, { kind: "task.result", corr: "t-a" }),
      env(3, { kind: "change_request", corr: "t-a" }),
    ];
    const cards = deriveRail(DAG, { "t-a": "assigned" }, messages, "run_1", null);
    const designer = cards.find((c) => c.role === "designer");
    expect(designer?.status).toBe("working");
    expect(designer?.currentTaskId).toBe("t-a");
  });

  it("returns to awaiting once a fresh task.result follows the change_request", () => {
    const messages = [
      assignMsg(1, "t-a", ["agent:designer"]),
      env(2, { kind: "task.result", corr: "t-a" }),
      env(3, { kind: "change_request", corr: "t-a" }),
      env(4, { kind: "task.result", corr: "t-a" }),
    ];
    const cards = deriveRail(DAG, { "t-a": "assigned" }, messages, "run_1", null);
    const designer = cards.find((c) => c.role === "designer");
    expect(designer?.status).toBe("awaiting");
  });
});

describe("deriveRail — boundary: empty messages / dag null / body shape mismatch", () => {
  it("shows all cards idle with no crash when there are no messages yet", () => {
    const cards = deriveRail(DAG, {}, [], null, null);
    expect(cards).toHaveLength(3); // lead + 2 roles present in DAG
    for (const card of cards) {
      expect(card.status).toBe("idle");
      expect(card.currentTaskId).toBeNull();
      expect(card.harness).toBe("claude-code");
    }
  });

  it("returns just the lead card with no crash when dag is null (pre-run)", () => {
    const cards = deriveRail(null, {}, [], null, null);
    expect(cards).toEqual([
      { id: "lead", role: "lead", status: "idle", currentTaskId: null, harness: "claude-code" },
    ]);
  });

  it("ignores a task.assign whose body doesn't match the {task:{id}} shape", () => {
    const messages = [
      env(1, { kind: "task.assign", corr: "t-a", body: { unexpected: true }, to: ["agent:designer"] }),
    ];
    const cards = deriveRail(DAG, { "t-a": "assigned" }, messages, "run_1", null);
    const designer = cards.find((c) => c.role === "designer");
    expect(designer?.status).toBe("idle");
    expect(designer?.currentTaskId).toBeNull();
  });

  it("lists every role appearing in dag.tasks and the lead, in a stable order", () => {
    const cards = deriveRail(DAG, {}, [], null, null);
    expect(cards.map((c) => c.role)).toEqual(["lead", "designer", "developer"]);
  });
});

describe("deriveRail — C7c avatar initials: 6 roles map to distinct 2-letter initials", () => {
  it("has exactly 6 entries with no duplicate values (lead/pm/designer/publisher/developer/qa)", () => {
    expect(Object.keys(AVATAR_INITIALS)).toHaveLength(6);
    expect(new Set(Object.values(AVATAR_INITIALS)).size).toBe(6);
    expect(AVATAR_INITIALS).toEqual({
      lead: "LD",
      pm: "PM",
      designer: "DS",
      publisher: "PB",
      developer: "DV",
      qa: "QA",
    });
  });

  it("falls back to the uppercased first 2 chars for a role not in the map", () => {
    expect(avatarInitials("scout")).toBe("SC");
  });
});

describe("deriveRail — C7c roster harness: normal / fallback / swap", () => {
  const roster: RosterAgentDto[] = [
    { id: "agent:lead", role: "lead", harness: "claude-code", model: "m" },
    { id: "agent:designer", role: "designer", harness: "opencode", model: "m" },
    { id: "agent:developer", role: "developer", harness: "claude-code", model: "m" },
  ];

  it("fills harness from the matching roster entry's role (normal)", () => {
    const messages = [assignMsg(1, "t-a", ["agent:designer"])];
    const cards = deriveRail(DAG, { "t-a": "assigned" }, messages, "run_1", null, roster);
    expect(cards.find((c) => c.role === "designer")?.harness).toBe("opencode");
    expect(cards.find((c) => c.role === "lead")?.harness).toBe("claude-code");
  });

  it('falls back to "claude-code" when roster is empty', () => {
    const messages = [assignMsg(1, "t-a", ["agent:designer"])];
    const cards = deriveRail(DAG, { "t-a": "assigned" }, messages, "run_1", null, []);
    for (const card of cards) {
      expect(card.harness).toBe("claude-code");
    }
  });

  it('falls back to "claude-code" for a role with no matching roster entry', () => {
    const messages = [assignMsg(1, "t-b", ["agent:developer"])];
    const noDeveloperRoster = roster.filter((a) => a.role !== "developer");
    const cards = deriveRail(DAG, { "t-b": "assigned" }, messages, "run_1", null, noDeveloperRoster);
    expect(cards.find((c) => c.role === "developer")?.harness).toBe("claude-code");
  });

  it("reflects a roster swap (roster_changed) on re-derivation without a re-render trick", () => {
    const messages = [assignMsg(1, "t-a", ["agent:designer"])];
    const before = deriveRail(DAG, { "t-a": "assigned" }, messages, "run_1", null, roster);
    expect(before.find((c) => c.role === "designer")?.harness).toBe("opencode");

    const swappedRoster = roster.map((a) => (a.role === "designer" ? { ...a, harness: "gemini-cli" } : a));
    const after = deriveRail(DAG, { "t-a": "assigned" }, messages, "run_1", null, swappedRoster);
    expect(after.find((c) => c.role === "designer")?.harness).toBe("gemini-cli");
  });
});

describe("deriveRail — full mock scenario replay", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("settles every worker and the lead to idle once the mock scenario finishes", async () => {
    const source = new MockEventSource(0);
    const store = createRunStore(source);
    source.onEvent(store.getState().applyEvent);

    await store.getState().startRun("간단한 랜딩 페이지");
    await vi.runAllTimersAsync();

    const s = store.getState();
    const cards = deriveRail(s.dag, s.taskStates, s.messages, s.runId, s.finished);
    expect(cards).toHaveLength(6); // lead + pm/designer/publisher/developer/qa
    for (const card of cards) {
      expect(card.status).toBe("idle");
    }
  });
});
