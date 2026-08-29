import { create } from "zustand";

import { createDefaultSource } from "./source";
import type { RunEventSource } from "./source";
import type { Envelope, RosterAgentDto, RunEvent, SpecDoc, TaskDag, TaskStateDto } from "./types";

/** Verbatim per contracts-m4.md §C4, extended per contracts-m5.md §C7a. */
export interface RunState {
  runId: string | null;
  goal: string | null;
  spec: SpecDoc | null;
  dag: TaskDag | null;
  sprint: string[];
  messages: { seq: number; envelope: Envelope }[];
  taskStates: Record<string, TaskStateDto>;
  finished: "completed" | "failed" | null;
  sprintIndex: number | null;
  sprintSummaries: { index: number; summary: string }[];
  sprintWindows: { index: number; startTs: string; endTs: string | null }[];
  roster: RosterAgentDto[];
  startRun(goal: string): Promise<void>;
  applyEvent(ev: RunEvent): void;
}

/**
 * Builds a fresh RunState store bound to `source` (D5: `startRun` delegates
 * to the injected `RunEventSource`; subscribing to it is the caller's job —
 * one `useEffect` wiring `source.onEvent(store.getState().applyEvent)`, not
 * done here).
 */
export function createRunStore(source: RunEventSource) {
  // Idempotency key set (D4) — kept outside the published RunState so the
  // store's public shape stays exactly the §C4 interface above.
  const seenSeqs = new Set<number>();

  return create<RunState>((set) => ({
    runId: null,
    goal: null,
    spec: null,
    dag: null,
    sprint: [],
    messages: [],
    taskStates: {},
    finished: null,
    sprintIndex: null,
    sprintSummaries: [],
    sprintWindows: [],
    roster: [],

    async startRun(goal) {
      await source.start(goal);
    },

    applyEvent(ev) {
      switch (ev.type) {
        case "run_started":
          seenSeqs.clear();
          set({
            runId: ev.run_id,
            goal: ev.goal,
            spec: null,
            dag: null,
            sprint: [],
            messages: [],
            taskStates: {},
            finished: null,
            sprintIndex: null,
            sprintSummaries: [],
            sprintWindows: [],
            roster: [],
          });
          return;

        case "spec_ready":
          set({ spec: ev.spec, dag: ev.dag, sprint: ev.sprint });
          return;

        case "message":
          if (seenSeqs.has(ev.seq)) return;
          seenSeqs.add(ev.seq);
          set((s) => ({ messages: [...s.messages, { seq: ev.seq, envelope: ev.envelope }] }));
          return;

        case "task_state_changed":
          set((s) => ({ taskStates: { ...s.taskStates, [ev.task_id]: ev.state } }));
          return;

        case "bus_lifecycle":
          // Not surfaced in RunState — lifecycle detail has no consumer yet.
          return;

        case "run_finished":
          set({ finished: ev.outcome });
          return;

        case "sprint_started":
          set({ sprintIndex: ev.index });
          // D3/D4: upsert into sprintWindows — startTs set/refreshed, endTs
          // left as-is if the window already exists (a finished window must
          // not be reopened by a re-applied sprint_started).
          set((s) => {
            const existing = s.sprintWindows.find((w) => w.index === ev.index);
            if (existing) {
              return {
                sprintWindows: s.sprintWindows.map((w) =>
                  w.index === ev.index ? { ...w, startTs: ev.ts } : w,
                ),
              };
            }
            return {
              sprintWindows: [...s.sprintWindows, { index: ev.index, startTs: ev.ts, endTs: null }].sort(
                (a, b) => a.index - b.index,
              ),
            };
          });
          return;

        case "sprint_finished":
          // Idempotent: same index re-applied is a no-op, not a duplicate append.
          set((s) =>
            s.sprintSummaries.some((entry) => entry.index === ev.index)
              ? {}
              : { sprintSummaries: [...s.sprintSummaries, { index: ev.index, summary: ev.summary }] },
          );
          // D3/D4: upsert into sprintWindows — set endTs if the window exists,
          // else create one with startTs=endTs=ev.ts (finished-before-started).
          set((s) => {
            const existing = s.sprintWindows.find((w) => w.index === ev.index);
            if (existing) {
              return {
                sprintWindows: s.sprintWindows.map((w) =>
                  w.index === ev.index ? { ...w, endTs: ev.ts } : w,
                ),
              };
            }
            return {
              sprintWindows: [...s.sprintWindows, { index: ev.index, startTs: ev.ts, endTs: ev.ts }].sort(
                (a, b) => a.index - b.index,
              ),
            };
          });
          return;

        case "roster_changed":
          // Full replace, never a merge (plan D2).
          set({ roster: ev.agents });
          return;

        default:
          // D6: unknown `type` from a newer wire version is ignored, not thrown.
          console.warn("applyEvent: ignoring unknown RunEvent type", ev);
      }
    },
  }));
}

export const defaultSource: RunEventSource = createDefaultSource();
export const useRunStore = createRunStore(defaultSource);
