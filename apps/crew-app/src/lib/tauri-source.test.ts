import { describe, expect, it, vi } from "vitest";

import { TauriEventSource } from "./tauri-source";
import type { Invoke, Listen } from "./tauri-source";
import type { Envelope, RunEvent, RunEventEnvelope } from "./types";

const ts = "2026-08-28T00:00:00Z";

function envelope(over: Partial<Envelope> = {}): Envelope {
  return {
    id: "env_1",
    ts,
    sprint: "sprint-1",
    thread: "t-pm",
    from: "agent:pm",
    to: ["lead"],
    kind: "task.result",
    corr: "t-pm",
    body: {},
    artifacts: [],
    requires_ack: false,
    deadline_ms: 60_000,
    ...over,
  };
}

/** A fake `listen` that hands the test its handler so it can push `{run_id, event}` envelopes on demand. */
function fakeListen(): { listen: Listen; push: (runId: string, ev: RunEvent) => void; unlisten: ReturnType<typeof vi.fn> } {
  let handler: ((payload: RunEventEnvelope) => void) | null = null;
  const unlisten = vi.fn();
  const listen: Listen = async (_event, h) => {
    handler = h;
    return unlisten;
  };
  return {
    listen,
    push: (runId, ev) => handler?.({ run_id: runId, event: ev }),
    unlisten,
  };
}

function snapshotFor(runId: string, over: Record<string, unknown> = {}) {
  return {
    run_id: runId,
    goal: "goal",
    spec: null,
    dag: null,
    sprint: [],
    task_states: [],
    messages: [],
    last_seq: 0,
    ts,
    ...over,
  };
}

describe("TauriEventSource.start — allocates the run only, no snapshot work", () => {
  it("invokes start_run with {goal, scripted, projectRoot} and returns the run_id, without calling run_snapshot", async () => {
    const { listen } = fakeListen();
    const invoke: Invoke = vi.fn(async (cmd, args) => {
      if (cmd === "start_run") {
        expect(args).toEqual({ goal: "landing page", scripted: true, projectRoot: "/ws/demo" });
        return "run-42" as never;
      }
      throw new Error(`unexpected invoke ${String(cmd)}`);
    });
    const source = new TauriEventSource(invoke, listen);

    const runId = await source.start("landing page", true, "/ws/demo");

    expect(runId).toBe("run-42");
  });

  it("still sends the projectRoot key when null, instead of omitting it (plan D6 — undocumented Tauri key-omission behavior)", async () => {
    const { listen } = fakeListen();
    const invoke: Invoke = vi.fn(async (cmd, args) => {
      if (cmd === "start_run") {
        expect(args).toBeDefined();
        expect("projectRoot" in (args as Record<string, unknown>)).toBe(true);
        expect((args as Record<string, unknown>).projectRoot).toBeNull();
        return "run-43" as never;
      }
      throw new Error(`unexpected invoke ${String(cmd)}`);
    });
    const source = new TauriEventSource(invoke, listen);

    const runId = await source.start("goal", false, null);

    expect(runId).toBe("run-43");
  });
});

describe("TauriEventSource.resync — restores one channel's state from run_snapshot(runId)", () => {
  it("replays the snapshot as synthesized events, in seq order, before any live event, gated to the resynced run_id", async () => {
    const { listen, push } = fakeListen();
    const invoke: Invoke = vi.fn(async (cmd, args) => {
      if (cmd === "run_snapshot") {
        expect(args).toEqual({ runId: "run-42" });
        return snapshotFor("run-42", {
          spec: { goal: "landing page", non_goals: [], constraints: [], requirements: [], acceptance: [] },
          dag: { tasks: [] },
          sprint: ["t-pm"],
          task_states: [["t-pm", "accepted"]],
          messages: [
            { seq: 2, envelope: envelope({ id: "env_2" }) },
            { seq: 1, envelope: envelope({ id: "env_1" }) },
          ],
          last_seq: 2,
        }) as never;
      }
      throw new Error(`unexpected invoke ${String(cmd)}`);
    });

    const source = new TauriEventSource(invoke, listen);
    const received: { runId: string; ev: RunEvent }[] = [];
    source.onEvent((runId, ev) => received.push({ runId, ev }));

    await source.resync("run-42");

    expect(received.map((r) => r.ev.type)).toEqual([
      "run_started",
      "spec_ready",
      "message",
      "message",
      "task_state_changed",
    ]);
    expect(received.every((r) => r.runId === "run-42")).toBe(true);
    const messages = received.map((r) => r.ev).filter((e): e is Extract<RunEvent, { type: "message" }> => e.type === "message");
    expect(messages.map((m) => m.seq)).toEqual([1, 2]);

    // A live message that duplicates the snapshot's last_seq is ignored.
    push("run-42", { type: "message", seq: 2, envelope: envelope({ id: "env_2" }) });
    expect(received).toHaveLength(5);

    // A live message past last_seq is delivered.
    push("run-42", { type: "message", seq: 3, envelope: envelope({ id: "env_3" }) });
    expect(received).toHaveLength(6);
    expect(received.at(-1)?.ev).toMatchObject({ type: "message", seq: 3 });
  });

  it("buffers a live event for the run_id currently mid-resync, delivering it only after the replay finishes (race-repro, plan D3)", async () => {
    const { listen, push } = fakeListen();
    let resolveSnapshot: (v: unknown) => void = () => {};
    let snapshotRequested: () => void = () => {};
    const snapshotRequestedPromise = new Promise<void>((resolve) => {
      snapshotRequested = resolve;
    });
    const invoke: Invoke = vi.fn(async (cmd) => {
      if (cmd === "run_snapshot") {
        snapshotRequested();
        return new Promise((resolve) => {
          resolveSnapshot = resolve;
        }) as never;
      }
      throw new Error(`unexpected invoke ${String(cmd)}`);
    });

    const source = new TauriEventSource(invoke, listen);
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    const resyncPromise = source.resync("run-1");
    await snapshotRequestedPromise;
    // Arrives while still replaying -> buffered, not dropped or delivered early.
    push("run-1", { type: "message", seq: 1, envelope: envelope() });
    expect(received).toHaveLength(0);

    resolveSnapshot(snapshotFor("run-1", { messages: [{ seq: 1, envelope: envelope() }], last_seq: 1 }));
    await resyncPromise;

    const messageEvents = received.filter((e) => e.type === "message");
    // Exactly one seq=1 message reaches subscribers — the snapshot's, not a duplicate from the buffer.
    expect(messageEvents).toHaveLength(1);
  });

  it("does not let one run's mid-replay buffering block or drop a different run's live event (R4 — no cross-run contamination)", async () => {
    const { listen, push } = fakeListen();
    let resolveSnapshot: (v: unknown) => void = () => {};
    let snapshotRequested: () => void = () => {};
    const snapshotRequestedPromise = new Promise<void>((resolve) => {
      snapshotRequested = resolve;
    });
    const invoke: Invoke = vi.fn(async (cmd, args) => {
      if (cmd === "run_snapshot") {
        if ((args as { runId: string }).runId === "run-a") {
          snapshotRequested();
          return new Promise((resolve) => {
            resolveSnapshot = resolve;
          }) as never;
        }
        return snapshotFor("run-b") as never;
      }
      throw new Error(`unexpected invoke ${String(cmd)}`);
    });

    const source = new TauriEventSource(invoke, listen);
    const received: { runId: string; ev: RunEvent }[] = [];
    source.onEvent((runId, ev) => received.push({ runId, ev }));

    const resyncA = source.resync("run-a"); // still in flight, replaying=true for run-a
    await snapshotRequestedPromise; // wait until run_snapshot({runId:"run-a"}) is actually in flight
    push("run-b", { type: "message", seq: 1, envelope: envelope() }); // unrelated run — delivered immediately
    expect(received.filter((r) => r.runId === "run-b")).toHaveLength(1);

    resolveSnapshot(snapshotFor("run-a"));
    await resyncA;
    expect(received.some((r) => r.runId === "run-a" && r.ev.type === "run_started")).toBe(true);
  });

  it("propagates a rejected run_snapshot (run_not_found) instead of swallowing it (error case)", async () => {
    const { listen } = fakeListen();
    const invoke: Invoke = vi.fn(async (cmd) => {
      if (cmd === "run_snapshot") throw new Error("run_not_found");
      throw new Error(`unexpected invoke ${String(cmd)}`);
    });
    const source = new TauriEventSource(invoke, listen);

    await expect(source.resync("run-gone")).rejects.toThrow("run_not_found");
  });

  it("still flushes any event buffered during a failed resync, instead of losing it (error/boundary)", async () => {
    const { listen, push } = fakeListen();
    let snapshotRequested: () => void = () => {};
    const snapshotRequestedPromise = new Promise<void>((resolve) => {
      snapshotRequested = resolve;
    });
    let rejectSnapshot: (err: unknown) => void = () => {};
    const invoke: Invoke = vi.fn(async (cmd) => {
      if (cmd === "run_snapshot") {
        snapshotRequested();
        return new Promise((_resolve, reject) => {
          rejectSnapshot = reject;
        }) as never;
      }
      throw new Error(`unexpected invoke ${String(cmd)}`);
    });
    const source = new TauriEventSource(invoke, listen);
    const received: RunEvent[] = [];
    source.onEvent((_runId, ev) => received.push(ev));

    const resyncPromise = source.resync("run-1").catch(() => {});
    await snapshotRequestedPromise;
    push("run-1", { type: "message", seq: 1, envelope: envelope() });
    rejectSnapshot(new Error("run_not_found"));
    await resyncPromise;

    expect(received).toHaveLength(1);
  });
});

describe("TauriEventSource — stop/remove/listRuns (multi-run commands)", () => {
  it("invokes stop_run/remove_run/list_runs with {runId} (or no args for list_runs)", async () => {
    const { listen } = fakeListen();
    const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      switch (cmd) {
        case "stop_run":
          expect(args).toEqual({ runId: "run-1" });
          return undefined;
        case "remove_run":
          expect(args).toEqual({ runId: "run-1" });
          return undefined;
        case "list_runs":
          expect(args).toBeUndefined();
          return [{ run_id: "run-1", goal: "g", finished: null }];
        default:
          throw new Error(`unexpected invoke ${cmd}`);
      }
    }) as unknown as Invoke;
    const source = new TauriEventSource(invoke, listen);

    await source.stop("run-1");
    await source.remove("run-1");
    await expect(source.listRuns()).resolves.toEqual([{ run_id: "run-1", goal: "g", finished: null }]);
  });

  it("unsubscribe stops a specific listener without affecting others", async () => {
    const { listen, push } = fakeListen();
    const invoke: Invoke = vi.fn(async () => undefined as never);
    const source = new TauriEventSource(invoke, listen);
    const receivedA: RunEvent[] = [];
    const receivedB: RunEvent[] = [];
    const unsubscribeA = source.onEvent((_r, ev) => receivedA.push(ev));
    source.onEvent((_r, ev) => receivedB.push(ev));

    await source.start("goal", false, null);
    unsubscribeA();
    push("run-x", { type: "message", seq: 1, envelope: envelope() });

    expect(receivedA).toHaveLength(0);
    expect(receivedB).toHaveLength(1);
  });
});

describe("TauriEventSource — C7a optional roster/harness methods (unchanged, no runId)", () => {
  it("invokes each command with the C6-verbatim name and forwards its resolved payload", async () => {
    const { listen } = fakeListen();
    const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      switch (cmd) {
        case "detect_harnesses":
          return [{ id: "claude-code", installed: true, path: "/bin/claude", adapter: "real" }];
        case "get_roster":
          return { agents: [] };
        case "set_roster":
          expect(args).toEqual({ roster: { agents: [] } });
          return undefined;
        case "list_presets":
          return [{ name: "클로드 5인팀", roster: { agents: [] } }];
        default:
          throw new Error(`unexpected invoke ${cmd}`);
      }
    }) as unknown as Invoke;
    const source = new TauriEventSource(invoke, listen);

    await expect(source.detectHarnesses!()).resolves.toEqual([
      { id: "claude-code", installed: true, path: "/bin/claude", adapter: "real" },
    ]);
    await expect(source.getRoster!()).resolves.toEqual({ agents: [] });
    await expect(source.setRoster!({ agents: [] })).resolves.toBeUndefined();
    await expect(source.listPresets!()).resolves.toEqual([{ name: "클로드 5인팀", roster: { agents: [] } }]);
  });
});

describe("TauriEventSource — D8: swapHarness/resolveGate/searchMessages take runId first", () => {
  it("invokes swap_harness/resolve_gate/search_messages with {runId, ...} and forwards the resolved payload", async () => {
    const { listen } = fakeListen();
    const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      switch (cmd) {
        case "swap_harness":
          expect(args).toEqual({ runId: "run-1", agentId: "agent:designer", harness: "opencode" });
          return undefined;
        case "resolve_gate":
          expect(args).toEqual({ runId: "run-1", taskId: "t-qa", decision: "approve", reason: "재현 확인" });
          return undefined;
        case "search_messages":
          expect(args).toEqual({ runId: "run-1", query: "handoff" });
          return [{ seq: 1, envelope: envelope() }];
        default:
          throw new Error(`unexpected invoke ${cmd}`);
      }
    }) as unknown as Invoke;
    const source = new TauriEventSource(invoke, listen);

    await expect(source.swapHarness!("run-1", "agent:designer", "opencode")).resolves.toBeUndefined();
    await expect(source.resolveGate!("run-1", "t-qa", "approve", "재현 확인")).resolves.toBeUndefined();
    await expect(source.searchMessages!("run-1", "handoff")).resolves.toEqual([{ seq: 1, envelope: envelope() }]);
  });

  it("propagates a rejected invoke instead of swallowing the error (boundary: run not found)", async () => {
    const { listen } = fakeListen();
    const invoke: Invoke = vi.fn(async () => {
      throw new Error("run_not_found");
    });
    const source = new TauriEventSource(invoke, listen);

    await expect(source.resolveGate!("run-gone", "t-qa", "reject", "반려")).rejects.toThrow("run_not_found");
  });
});

describe("TauriEventSource.createProject", () => {
  it("invokes create_project with {name} and forwards the resolved ProjectInfo", async () => {
    const { listen } = fakeListen();
    const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "create_project") {
        expect(args).toEqual({ name: "my-app" });
        return { name: "my-app", path: "/workspace/my-app" };
      }
      throw new Error(`unexpected invoke ${cmd}`);
    }) as unknown as Invoke;
    const source = new TauriEventSource(invoke, listen);

    await expect(source.createProject("my-app")).resolves.toEqual({ name: "my-app", path: "/workspace/my-app" });
  });

  it("propagates a rejected create_project (error vocabulary) instead of swallowing it", async () => {
    const { listen } = fakeListen();
    const invoke: Invoke = vi.fn(async () => {
      throw new Error("gh_missing");
    });
    const source = new TauriEventSource(invoke, listen);

    await expect(source.createProject("my-app")).rejects.toThrow("gh_missing");
  });
});

describe("TauriEventSource.listProjects", () => {
  it("invokes list_projects with no args and forwards the resolved ProjectInfo[]", async () => {
    const { listen } = fakeListen();
    const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "list_projects") {
        expect(args).toBeUndefined();
        return [
          { name: "alpha", path: "/ws/alpha" },
          { name: "beta", path: "/ws/beta" },
        ];
      }
      throw new Error(`unexpected invoke ${cmd}`);
    }) as unknown as Invoke;
    const source = new TauriEventSource(invoke, listen);

    await expect(source.listProjects!()).resolves.toEqual([
      { name: "alpha", path: "/ws/alpha" },
      { name: "beta", path: "/ws/beta" },
    ]);
  });

  it("propagates a rejected list_projects (e.g. workspace_missing) instead of swallowing it", async () => {
    const { listen } = fakeListen();
    const invoke: Invoke = vi.fn(async () => {
      throw new Error("workspace_missing: /ws");
    });
    const source = new TauriEventSource(invoke, listen);

    await expect(source.listProjects!()).rejects.toThrow("workspace_missing: /ws");
  });

  it("resolves an empty array when the workspace root exists but has zero projects (boundary)", async () => {
    const { listen } = fakeListen();
    const invoke: Invoke = vi.fn(async () => [] as never);
    const source = new TauriEventSource(invoke, listen);

    await expect(source.listProjects!()).resolves.toEqual([]);
  });
});
