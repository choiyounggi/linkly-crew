import { describe, expect, it, vi } from "vitest";

import { TauriEventSource } from "./tauri-source";
import type { Invoke, Listen } from "./tauri-source";
import type { Envelope, RunEvent } from "./types";

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

/** A fake `listen` that hands the test its handler so it can push events on demand. */
function fakeListen(): { listen: Listen; push: (ev: RunEvent) => void; unlisten: ReturnType<typeof vi.fn> } {
  let handler: ((payload: RunEvent) => void) | null = null;
  const unlisten = vi.fn();
  const listen: Listen = async (_event, h) => {
    handler = h;
    return unlisten;
  };
  return {
    listen,
    push: (ev) => handler?.(ev),
    unlisten,
  };
}

describe("TauriEventSource", () => {
  it("restores state from run_snapshot as synthesized events, in seq order, before any live event", async () => {
    const { listen, push } = fakeListen();
    const invoke: Invoke = vi.fn(async (cmd) => {
      if (cmd === "start_run") return "run-42" as never;
      if (cmd === "run_snapshot") {
        return {
          run_id: "run-42",
          goal: "landing page",
          spec: { goal: "landing page", non_goals: [], constraints: [], requirements: [], acceptance: [] },
          dag: { tasks: [] },
          sprint: ["t-pm"],
          task_states: [["t-pm", "accepted"]],
          messages: [
            { seq: 2, envelope: envelope({ id: "env_2" }) },
            { seq: 1, envelope: envelope({ id: "env_1" }) },
          ],
          last_seq: 2,
          ts,
        } as never;
      }
      throw new Error(`unexpected invoke ${cmd}`);
    });

    const source = new TauriEventSource(invoke, listen);
    const received: RunEvent[] = [];
    source.onEvent((ev) => received.push(ev));

    await source.start("landing page");

    expect(received.map((e) => e.type)).toEqual([
      "run_started",
      "spec_ready",
      "message",
      "message",
      "task_state_changed",
    ]);
    const messages = received.filter((e): e is Extract<RunEvent, { type: "message" }> => e.type === "message");
    expect(messages.map((m) => m.seq)).toEqual([1, 2]);

    // A live message that duplicates the snapshot's last_seq is ignored.
    push({ type: "message", seq: 2, envelope: envelope({ id: "env_2" }) });
    expect(received).toHaveLength(5);

    // A live message past last_seq is delivered.
    push({ type: "message", seq: 3, envelope: envelope({ id: "env_3" }) });
    expect(received).toHaveLength(6);
    expect(received.at(-1)).toMatchObject({ type: "message", seq: 3 });
  });

  it("ignores a live message at or below the snapshot's last_seq, even one buffered before the snapshot replay", async () => {
    const { listen, push } = fakeListen();
    let resolveSnapshot: (v: unknown) => void = () => {};
    let snapshotRequested: () => void = () => {};
    const snapshotRequestedPromise = new Promise<void>((resolve) => {
      snapshotRequested = resolve;
    });
    const invoke: Invoke = vi.fn(async (cmd) => {
      if (cmd === "start_run") return "run-1" as never;
      if (cmd === "run_snapshot") {
        // Signals the test that `start()` is now suspended awaiting this
        // call, i.e. still "replaying" — the exact window the buffer exists for.
        snapshotRequested();
        return new Promise((resolve) => {
          resolveSnapshot = resolve;
        }) as never;
      }
      throw new Error(`unexpected invoke ${cmd}`);
    });

    const source = new TauriEventSource(invoke, listen);
    const received: RunEvent[] = [];
    source.onEvent((ev) => received.push(ev));

    const startPromise = source.start("goal");
    await snapshotRequestedPromise;
    // Arrives while still "replaying" (before run_snapshot resolves) -> buffered.
    push({ type: "message", seq: 1, envelope: envelope() });

    resolveSnapshot({
      run_id: "run-1",
      goal: "goal",
      spec: null,
      dag: null,
      sprint: [],
      task_states: [],
      messages: [{ seq: 1, envelope: envelope() }],
      last_seq: 1,
      ts,
    });
    await startPromise;

    const messageEvents = received.filter((e) => e.type === "message");
    // Exactly one seq=1 message reaches subscribers — the snapshot's, not a duplicate from the buffer.
    expect(messageEvents).toHaveLength(1);
  });

  it("cleans up the Tauri listener when start_run rejects, instead of leaving it registered", async () => {
    const { listen, push, unlisten } = fakeListen();
    const invoke: Invoke = vi.fn(async (cmd) => {
      if (cmd === "start_run") throw new Error("run_in_progress");
      throw new Error(`unexpected invoke ${cmd}`);
    });

    const source = new TauriEventSource(invoke, listen);
    const received: RunEvent[] = [];
    source.onEvent((ev) => received.push(ev));

    await expect(source.start("goal")).rejects.toThrow("run_in_progress");
    expect(unlisten).toHaveBeenCalledTimes(1);

    // The (now stale) underlying listener firing after the failed start
    // must not reach subscribers — it was torn down, not merely buffered.
    push({ type: "message", seq: 1, envelope: envelope() });
    expect(received).toHaveLength(0);
  });

  it("unsubscribe stops a specific listener without affecting others or the underlying Tauri listener", async () => {
    const { listen, push, unlisten } = fakeListen();
    const invoke: Invoke = vi.fn(async (cmd) => {
      if (cmd === "start_run") return "run-1" as never;
      if (cmd === "run_snapshot") {
        return {
          run_id: "run-1",
          goal: "goal",
          spec: null,
          dag: null,
          sprint: [],
          task_states: [],
          messages: [],
          last_seq: 0,
          ts,
        } as never;
      }
      if (cmd === "stop_run") return undefined as never;
      throw new Error(`unexpected invoke ${cmd}`);
    });

    const source = new TauriEventSource(invoke, listen);
    const receivedA: RunEvent[] = [];
    const receivedB: RunEvent[] = [];
    const unsubscribeA = source.onEvent((ev) => receivedA.push(ev));
    source.onEvent((ev) => receivedB.push(ev));

    await source.start("goal");
    unsubscribeA();

    push({ type: "message", seq: 1, envelope: envelope() });
    expect(receivedA).toHaveLength(1); // only run_started from before unsubscribe
    expect(receivedB).toHaveLength(2); // run_started + the live message

    expect(unlisten).not.toHaveBeenCalled();
    await source.stop();
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("stop_run");
  });
});

describe("TauriEventSource — C7a optional roster/harness methods", () => {
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
        case "swap_harness":
          expect(args).toEqual({ agentId: "agent:designer", harness: "opencode" });
          return undefined;
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
    await expect(source.swapHarness!("agent:designer", "opencode")).resolves.toBeUndefined();

    expect(invoke).toHaveBeenCalledWith("detect_harnesses");
    expect(invoke).toHaveBeenCalledWith("get_roster");
  });

  it("propagates a rejected invoke instead of swallowing the error (boundary: backend not implemented yet)", async () => {
    const { listen } = fakeListen();
    const invoke: Invoke = vi.fn(async () => {
      throw new Error("command swap_harness not found");
    });

    const source = new TauriEventSource(invoke, listen);

    await expect(source.swapHarness!("agent:designer", "opencode")).rejects.toThrow("command swap_harness not found");
  });
});
