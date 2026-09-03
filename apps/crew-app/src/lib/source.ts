import { isTauri } from "@tauri-apps/api/core";

import type { Envelope, HarnessInfo, ProjectInfo, Roster, RosterPreset, RunEvent, RunSummary } from "./types";
import { MockEventSource } from "./mock-source";
import { TauriEventSource } from "./tauri-source";

/**
 * Multi-run source interface (plan D1/D3/D8, extending contracts-m4.md §C4,
 * contracts-m5.md §C7a, contracts-m7.md §E8 for the run_id-routed backend —
 * t1-be-multirun's MultiRunApi). `t-bridge` implements `TauriEventSource`
 * (`src/lib/tauri-source.ts`) against this interface. The optional methods
 * are optional so a source that doesn't support roster/harness editing
 * still compiles.
 */
export interface RunEventSource {
  /** start_run(goal, scripted, projectRoot) -> run_id. Only allocates the run; no snapshot work (call `resync` after inserting the channel). `projectRoot` is `null` for the legacy scratch run (plan D1/D6, 2차 런 이슈 #13). */
  start(goal: string, scripted: boolean, projectRoot: string | null): Promise<string>;
  /** Returns an unsubscribe function. Every event is tagged with its run_id (plan D3) so a caller can route/ignore per channel. */
  onEvent(cb: (runId: string, ev: RunEvent) => void): () => void;
  stop(runId: string): Promise<void>;
  remove(runId: string): Promise<void>;
  /** Re-syncs one channel's state from the backend's current snapshot (plan D3) — called after `start` and on channel (re)selection. */
  resync(runId: string): Promise<void>;
  listRuns(): Promise<RunSummary[]>;
  /** t3-be-project's ProjectApi (decisions.md) — the new-task flow's first step (plan D4), ahead of `start`. */
  createProject(name: string): Promise<ProjectInfo>;
  /**
   * list_projects() -> ProjectInfo[], name-ascending (plan D5, 2차 런 이슈 #13). Optional so
   * sources without the "기존 선택" flow still compile (t2-fe-picker's `fakeSource` fan-out
   * guard). Rejects `workspace_missing: <path>` if the workspace root is missing/not a
   * directory; resolves `[]` if the root exists with zero projects.
   */
  listProjects?(): Promise<ProjectInfo[]>;
  /** D8: run_id-first, per t1's MultiRunApi contract (decisions.md). */
  swapHarness?(runId: string, agentId: string, harness: string): Promise<void>;
  getRoster?(): Promise<Roster>;
  setRoster?(roster: Roster): Promise<void>;
  listPresets?(): Promise<RosterPreset[]>;
  detectHarnesses?(): Promise<HarnessInfo[]>;
  /** D8: run_id-first. */
  resolveGate?(runId: string, taskId: string, decision: "approve" | "reject", reason: string): Promise<void>;
  /** D8: run_id-first. */
  searchMessages?(runId: string, query: string): Promise<{ seq: number; envelope: Envelope }[]>;
}

/**
 * Detects a Tauri runtime (`@tauri-apps/api/core`'s `isTauri()`, contracts-m4.md
 * §C6 / plan D7) and returns a `TauriEventSource` against it; otherwise (plain
 * browser dev, component tests) a `MockEventSource`.
 */
export function createDefaultSource(): RunEventSource {
  return isTauri() ? new TauriEventSource() : new MockEventSource();
}
