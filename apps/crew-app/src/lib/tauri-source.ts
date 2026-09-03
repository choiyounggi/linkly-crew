import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type { RunEventSource } from "./source";
import type {
  Envelope,
  HarnessInfo,
  ProjectInfo,
  Roster,
  RosterPreset,
  RunEvent,
  RunEventEnvelope,
  RunSummary,
  SpecDoc,
  TaskDag,
  TaskStateDto,
} from "./types";

const RUN_EVENT_CHANNEL = "run://event";

/** `invoke`'s shape, narrowed to what this source calls — injectable for tests (plan D6). */
export type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
/** `listen`'s shape, narrowed to a plain payload callback — injectable for tests (plan D6). */
export type Listen = (event: string, handler: (payload: RunEventEnvelope) => void) => Promise<UnlistenFn>;

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

function defaultListen(event: string, handler: (payload: RunEventEnvelope) => void): Promise<UnlistenFn> {
  return tauriListen<RunEventEnvelope>(event, (e) => handler(e.payload));
}

/** Snapshot -> synthesized `RunEvent`s, `run_started`/`spec_ready` first, then `message`s in seq order, then `task_state_changed`s (plan D6, extended by plan D3 to run per run_id). */
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
 * `RunEventSource` over the Tauri bridge (contracts-m4.md §C6, extended by
 * t1-be-multirun's MultiRunApi + plan D3): a single `"run://event"` listener,
 * registered once and shared across every run, dispatches `{run_id, event}`
 * to subscribers by run_id — no per-run listener lifecycle. Each run_id gets
 * its own replay-buffer/seq-dedup gate (`replaying`/`buffered`/`lastSeq`,
 * keyed by run_id) so a live event arriving mid-`resync` for THAT run is
 * queued, not dropped or delivered out of snapshot order; a live event for a
 * different run_id is unaffected (plan D3/R4 — no cross-run contamination).
 */
export class TauriEventSource implements RunEventSource {
  private readonly invoke: Invoke;
  private readonly listen: Listen;
  private readonly listeners = new Set<(runId: string, ev: RunEvent) => void>();
  private unlisten: UnlistenFn | null = null;
  private readonly lastSeq = new Map<string, number>();
  private readonly replaying = new Map<string, boolean>();
  private readonly buffered = new Map<string, RunEvent[]>();

  constructor(invoke: Invoke = defaultInvoke, listen: Listen = defaultListen) {
    this.invoke = invoke;
    this.listen = listen;
  }

  private async ensureListening(): Promise<void> {
    if (this.unlisten) return;
    this.unlisten = await this.listen(RUN_EVENT_CHANNEL, (envelope) => {
      this.route(envelope.run_id, envelope.event);
    });
  }

  /** Allocates the run and returns its id — no snapshot work (call `resync` once the caller has a channel to deliver into). `projectRoot` is always sent as an explicit key, `null` for the legacy scratch run — the backend contract does not define key-omission behavior (plan D6). */
  async start(goal: string, scripted: boolean, projectRoot: string | null): Promise<string> {
    await this.ensureListening();
    return this.invoke<string>("start_run", { goal, scripted, projectRoot });
  }

  /** Fetches `run_snapshot(runId)`, replays it as synthesized events, then flushes any live event that arrived during the replay (plan D3/D6). Also ensures the shared listener is active — a channel can be resynced (e.g. after reload, via `listRuns`) without `start` ever having run this session. */
  async resync(runId: string): Promise<void> {
    await this.ensureListening();
    this.lastSeq.set(runId, 0);
    this.replaying.set(runId, true);
    this.buffered.set(runId, []);

    try {
      const snapshot = await this.invoke<RunSnapshotDto>("run_snapshot", { runId });

      for (const ev of synthesizeSnapshotEvents(snapshot)) {
        this.emit(runId, ev);
      }
      // The max seq actually replayed (0 if none) — not `snapshot.last_seq`,
      // which can disagree with `snapshot.messages` under a backend race
      // (t1-msg-race plan D3). Deriving `lastSeq` from what was truly
      // replayed makes the dedup gate below self-consistent regardless of
      // that field's value.
      this.lastSeq.set(
        runId,
        snapshot.messages.reduce((max, m) => Math.max(max, m.seq), 0),
      );
    } finally {
      this.replaying.set(runId, false);
      const buf = this.buffered.get(runId) ?? [];
      this.buffered.delete(runId);
      for (const ev of buf) {
        this.deliverLive(runId, ev);
      }
    }
  }

  onEvent(cb: (runId: string, ev: RunEvent) => void): () => void {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  }

  async stop(runId: string): Promise<void> {
    await this.invoke<void>("stop_run", { runId });
  }

  async remove(runId: string): Promise<void> {
    // Drop this run's per-channel dedup/replay-gate bookkeeping regardless of
    // the invoke's outcome — otherwise it accumulates for the lifetime of
    // the source across a long session's worth of closed channels.
    try {
      await this.invoke<void>("remove_run", { runId });
    } finally {
      this.lastSeq.delete(runId);
      this.replaying.delete(runId);
      this.buffered.delete(runId);
    }
  }

  async listRuns(): Promise<RunSummary[]> {
    return this.invoke<RunSummary[]>("list_runs");
  }

  async createProject(name: string): Promise<ProjectInfo> {
    return this.invoke<ProjectInfo>("create_project", { name });
  }

  /** Rejects with the backend's error as-is (e.g. `workspace_missing: <path>`) — not wrapped, so callers can branch on it. */
  async listProjects(): Promise<ProjectInfo[]> {
    return this.invoke<ProjectInfo[]>("list_projects");
  }

  /** Command names verbatim per contracts-m5.md §C6; D8 adds `runId` first. Errors reject, not swallowed. */
  async swapHarness(runId: string, agentId: string, harness: string): Promise<void> {
    await this.invoke<void>("swap_harness", { runId, agentId, harness });
  }

  async getRoster(): Promise<Roster> {
    return this.invoke<Roster>("get_roster");
  }

  async setRoster(roster: Roster): Promise<void> {
    await this.invoke<void>("set_roster", { roster });
  }

  async listPresets(): Promise<RosterPreset[]> {
    return this.invoke<RosterPreset[]>("list_presets");
  }

  async detectHarnesses(): Promise<HarnessInfo[]> {
    return this.invoke<HarnessInfo[]>("detect_harnesses");
  }

  /** Command names verbatim per contracts-m7.md §E8; D8 adds `runId` first. */
  async resolveGate(runId: string, taskId: string, decision: "approve" | "reject", reason: string): Promise<void> {
    await this.invoke<void>("resolve_gate", { runId, taskId, decision, reason });
  }

  async searchMessages(runId: string, query: string): Promise<{ seq: number; envelope: Envelope }[]> {
    return this.invoke<{ seq: number; envelope: Envelope }[]>("search_messages", { runId, query });
  }

  /** Live-event router (plan D3): buffers for the run_id currently mid-`resync`, delivers everything else immediately. Unaffected run_ids are never blocked by another run's replay. */
  private route(runId: string, ev: RunEvent): void {
    if (this.replaying.get(runId)) {
      const buf = this.buffered.get(runId);
      if (buf) buf.push(ev);
      else this.buffered.set(runId, [ev]);
      return;
    }
    this.deliverLive(runId, ev);
  }

  /** A live `message` at or below this run's `lastSeq` is already covered by its snapshot replay — dropped, not re-delivered. */
  private deliverLive(runId: string, ev: RunEvent): void {
    if (ev.type === "message") {
      const last = this.lastSeq.get(runId) ?? 0;
      if (ev.seq <= last) return;
      this.lastSeq.set(runId, ev.seq);
    }
    this.emit(runId, ev);
  }

  private emit(runId: string, ev: RunEvent): void {
    for (const cb of this.listeners) cb(runId, ev);
  }
}
