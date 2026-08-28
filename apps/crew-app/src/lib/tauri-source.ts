import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type { RunEventSource } from "./source";
import type { Envelope, RunEvent, SpecDoc, TaskDag, TaskStateDto } from "./types";

const RUN_EVENT_CHANNEL = "run://event";

/** `invoke`'s shape, narrowed to what this source calls — injectable for tests (plan D6). */
export type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
/** `listen`'s shape, narrowed to a plain payload callback — injectable for tests (plan D6). */
export type Listen = (event: string, handler: (payload: RunEvent) => void) => Promise<UnlistenFn>;

/** `RunSnapshot` (contracts-m4.md §C3) — the `run_snapshot` command's payload. */
interface RunSnapshotDto {
  run_id: string;
  goal: string;
  spec: SpecDoc | null;
  dag: TaskDag | null;
  sprint: string[];
  task_states: [string, TaskStateDto][];
  messages: { seq: number; envelope: Envelope }[];
  last_seq: number;
  ts: string;
}

function defaultInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return tauriInvoke<T>(cmd, args);
}

function defaultListen(event: string, handler: (payload: RunEvent) => void): Promise<UnlistenFn> {
  return tauriListen<RunEvent>(event, (e) => handler(e.payload));
}

/** Snapshot -> synthesized `RunEvent`s, `run_started`/`spec_ready` first, then `message`s in seq order, then `task_state_changed`s (plan D6). */
function synthesizeSnapshotEvents(snapshot: RunSnapshotDto): RunEvent[] {
  const events: RunEvent[] = [
    { type: "run_started", run_id: snapshot.run_id, goal: snapshot.goal, ts: snapshot.ts },
  ];

  if (snapshot.spec && snapshot.dag) {
    events.push({
      type: "spec_ready",
      spec: snapshot.spec,
      dag: snapshot.dag,
      sprint: snapshot.sprint,
      ts: snapshot.ts,
    });
  }

  for (const m of [...snapshot.messages].sort((a, b) => a.seq - b.seq)) {
    events.push({ type: "message", seq: m.seq, envelope: m.envelope });
  }

  for (const [task_id, state] of snapshot.task_states) {
    events.push({ type: "task_state_changed", task_id, state, ts: snapshot.ts });
  }

  return events;
}

/**
 * `RunEventSource` over the Tauri bridge (contracts-m4.md §C6): `invoke`s
 * `start_run`/`stop_run`, `listen`s on `"run://event"`. `listen` is
 * registered before `start_run` is invoked so no event between registration
 * and the snapshot restore below is ever lost — every live event received
 * during that window is buffered, then flushed once the snapshot replay has
 * set `lastSeq`, applying the same seq dedup as the live stream from then on
 * (plan D6, frontend/data-fetching/race-conditions.md).
 */
export class TauriEventSource implements RunEventSource {
  private readonly invoke: Invoke;
  private readonly listen: Listen;
  private readonly listeners = new Set<(ev: RunEvent) => void>();
  private unlisten: UnlistenFn | null = null;
  private lastSeq = 0;

  constructor(invoke: Invoke = defaultInvoke, listen: Listen = defaultListen) {
    this.invoke = invoke;
    this.listen = listen;
  }

  async start(goal: string): Promise<void> {
    this.lastSeq = 0;
    let replaying = true;
    const buffered: RunEvent[] = [];

    this.unlisten = await this.listen(RUN_EVENT_CHANNEL, (ev) => {
      if (replaying) {
        buffered.push(ev);
        return;
      }
      this.deliverLive(ev);
    });

    try {
      await this.invoke<string>("start_run", { goal, scripted: true });
      const snapshot = await this.invoke<RunSnapshotDto>("run_snapshot");

      for (const ev of synthesizeSnapshotEvents(snapshot)) {
        this.emit(ev);
      }
      this.lastSeq = snapshot.last_seq;

      replaying = false;
      for (const ev of buffered) {
        this.deliverLive(ev);
      }
    } catch (err) {
      // start_run/run_snapshot failed (e.g. "run_in_progress") — the run
      // never started under this source, so the listener registered above
      // must not linger: left alive, it would go on buffering (and never
      // flushing) or, once GC'd, could still fire for a stray event.
      this.unlisten();
      this.unlisten = null;
      throw err;
    }
  }

  onEvent(cb: (ev: RunEvent) => void): () => void {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  }

  async stop(): Promise<void> {
    if (this.unlisten) {
      this.unlisten();
      this.unlisten = null;
    }
    await this.invoke<void>("stop_run");
  }

  /** A live `message` at or below `lastSeq` is already covered by the snapshot replay — dropped, not re-delivered. */
  private deliverLive(ev: RunEvent): void {
    if (ev.type === "message") {
      if (ev.seq <= this.lastSeq) return;
      this.lastSeq = ev.seq;
    }
    this.emit(ev);
  }

  private emit(ev: RunEvent): void {
    for (const cb of this.listeners) cb(ev);
  }
}
