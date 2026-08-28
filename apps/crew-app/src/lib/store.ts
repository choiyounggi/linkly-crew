import { create } from "zustand";

import { createDefaultSource } from "./source";
import type { RunEventSource } from "./source";
import type { Envelope, RunEvent, SpecDoc, TaskDag, TaskStateDto } from "./types";

/** Verbatim per contracts-m4.md §C4. */
export interface RunState {
  runId: string | null;
  goal: string | null;
  spec: SpecDoc | null;
  dag: TaskDag | null;
  sprint: string[];
  messages: { seq: number; envelope: Envelope }[];
  taskStates: Record<string, TaskStateDto>;
  finished: "completed" | "failed" | null;
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

        default:
          // D6: unknown `type` from a newer wire version is ignored, not thrown.
          console.warn("applyEvent: ignoring unknown RunEvent type", ev);
      }
    },
  }));
}

export const defaultSource: RunEventSource = createDefaultSource();
export const useRunStore = createRunStore(defaultSource);
