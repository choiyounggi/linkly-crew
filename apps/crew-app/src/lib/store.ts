import { create } from "zustand";

import { createDefaultSource } from "./source";
import type { RunEventSource } from "./source";
import type { Envelope, RosterAgentDto, RunEvent, SpecDoc, TaskDag, TaskStateDto } from "./types";

/**
 * One channel = one run (plan D1). Shape mirrors the old single-run
 * `RunState` fields 1:1 (contracts-m4.md §C4 / §C7a), just keyed by `runId`
 * instead of being the store root. Derived values (labels, status badges,
 * counts) are computed by consumers from these fields, never stored here
 * (plan D2 / wiki/frontend/state/derived-state.md).
 */
export interface ChannelState {
  runId: string;
  goal: string;
  scripted: boolean;
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
  /** t7 plan D4: agent ids that have read each message, keyed by message id. Reset on resync (run_started). */
  readReceipts: Record<string, string[]>;
  /** t7 plan D5: whether each agent is currently typing. Reset on resync (run_started). */
  typing: Record<string, boolean>;
  /** t7 plan D1 (Task 03): thread id open in the right-panel ThreadPanel, or null when none. Set directly via `useRunStore.setState` from chat/index.tsx — no dedicated action, per this task's field-only store scope. */
  selectedThread: string | null;
}

/** Multi-run store root (plan D1): `channels` keyed by `run_id`, `channelOrder` for sidebar ordering, `activeRunId` for the selected channel. */
export interface RunState {
  channels: Record<string, ChannelState>;
  activeRunId: string | null;
  channelOrder: string[];
  /** create_project→start_run flow's last step (plan D4) — resolves once the run exists, selects it, returns its run_id. */
  startChannel(goal: string, scripted: boolean): Promise<string>;
  /** Selects a channel and re-syncs its state from the backend (plan D3). */
  selectChannel(runId: string): void;
  /** stop_run + remove_run (plan Task01 step1), then drops the channel locally regardless of either call's outcome. */
  closeChannel(runId: string): Promise<void>;
  /** Routes an event to its channel only; an unknown run_id is ignored + warned (plan D3), never auto-creates a channel. */
  applyEvent(runId: string, ev: RunEvent): void;
}

/** Exported for tests that need a fully-shaped `ChannelState` (e.g. features/chat's). */
export function emptyChannel(runId: string, goal: string, scripted: boolean): ChannelState {
  return {
    runId,
    goal,
    scripted,
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
    readReceipts: {},
    typing: {},
    selectedThread: null,
  };
}

/** Applies one `RunEvent` to one channel's state, returning a new object (or the same reference for a no-op — callers use `===` to skip a `set`). */
function applyEventToChannel(
  channel: ChannelState,
  ev: RunEvent,
  seenSeqs: Set<number>,
): ChannelState {
  switch (ev.type) {
    case "run_started":
      seenSeqs.clear();
      return emptyChannel(channel.runId, ev.goal, channel.scripted);

    case "spec_ready":
      return { ...channel, spec: ev.spec, dag: ev.dag, sprint: ev.sprint };

    case "message":
      if (seenSeqs.has(ev.seq)) return channel;
      seenSeqs.add(ev.seq);
      return { ...channel, messages: [...channel.messages, { seq: ev.seq, envelope: ev.envelope }] };

    case "task_state_changed":
      return { ...channel, taskStates: { ...channel.taskStates, [ev.task_id]: ev.state } };

    case "bus_lifecycle":
      // Not surfaced in ChannelState — lifecycle detail has no consumer yet.
      return channel;

    case "run_finished":
      return { ...channel, finished: ev.outcome };

    case "sprint_started": {
      const existing = channel.sprintWindows.find((w) => w.index === ev.index);
      const sprintWindows = existing
        ? channel.sprintWindows.map((w) => (w.index === ev.index ? { ...w, startTs: ev.ts } : w))
        : [...channel.sprintWindows, { index: ev.index, startTs: ev.ts, endTs: null }].sort(
            (a, b) => a.index - b.index,
          );
      return { ...channel, sprintIndex: ev.index, sprintWindows };
    }

    case "sprint_finished": {
      const sprintSummaries = channel.sprintSummaries.some((entry) => entry.index === ev.index)
        ? channel.sprintSummaries
        : [...channel.sprintSummaries, { index: ev.index, summary: ev.summary }];
      const existing = channel.sprintWindows.find((w) => w.index === ev.index);
      const sprintWindows = existing
        ? channel.sprintWindows.map((w) => (w.index === ev.index ? { ...w, endTs: ev.ts } : w))
        : [...channel.sprintWindows, { index: ev.index, startTs: ev.ts, endTs: ev.ts }].sort(
            (a, b) => a.index - b.index,
          );
      return { ...channel, sprintSummaries, sprintWindows };
    }

    case "roster_changed":
      // Full replace, never a merge (plan D2).
      return { ...channel, roster: ev.agents };

    case "presence":
      if (ev.kind === "read") {
        // target_msg_id is optional on the wire (t2 stub) — no-op if absent (plan D4).
        if (!ev.target_msg_id) return channel;
        const readers = channel.readReceipts[ev.target_msg_id] ?? [];
        if (readers.includes(ev.agent_id)) return channel;
        return {
          ...channel,
          readReceipts: { ...channel.readReceipts, [ev.target_msg_id]: [...readers, ev.agent_id] },
        };
      }
      // kind === "typing"; active is optional on the wire (t2 stub) — absent means off (plan D5).
      return { ...channel, typing: { ...channel.typing, [ev.agent_id]: Boolean(ev.active) } };

    default:
      // D6: unknown `type` from a newer wire version is ignored, not thrown.
      console.warn("applyEvent: ignoring unknown RunEvent type", ev);
      return channel;
  }
}

/**
 * Builds a fresh multi-run store bound to `source`. Subscribing to the
 * source is the caller's job — one `useEffect` wiring
 * `source.onEvent(store.getState().applyEvent)` (unchanged from the
 * single-run shape), not done here.
 */
export function createRunStore(source: RunEventSource) {
  // Per-channel idempotency key sets, kept outside the published RunState
  // (mirrors the old single-run `seenSeqs`, now one Set per run_id) so the
  // store's public shape stays exactly the ChannelState fields above.
  const seenSeqsByRun = new Map<string, Set<number>>();

  function seenSeqsFor(runId: string): Set<number> {
    let seen = seenSeqsByRun.get(runId);
    if (!seen) {
      seen = new Set();
      seenSeqsByRun.set(runId, seen);
    }
    return seen;
  }

  return create<RunState>((set, get) => ({
    channels: {},
    activeRunId: null,
    channelOrder: [],

    async startChannel(goal, scripted) {
      // source.start() only allocates the run and returns its id (no
      // snapshot work) — the channel entry below must exist before
      // source.resync() below can deliver any event for it, or those
      // events would be dropped as "unknown run_id" (plan D3).
      const runId = await source.start(goal, scripted);
      seenSeqsByRun.set(runId, new Set());
      set((s) => ({
        channels: { ...s.channels, [runId]: emptyChannel(runId, goal, scripted) },
        channelOrder: [...s.channelOrder, runId],
        activeRunId: runId,
      }));
      await source.resync(runId);
      return runId;
    },

    selectChannel(runId) {
      set({ activeRunId: runId });
      source.resync(runId).catch((err: unknown) => {
        console.warn("selectChannel: resync failed", runId, err);
      });
    },

    async closeChannel(runId) {
      try {
        await source.stop(runId);
      } catch (err) {
        console.warn("closeChannel: stop_run failed", runId, err);
      }
      try {
        await source.remove(runId);
      } catch (err) {
        console.warn("closeChannel: remove_run failed", runId, err);
      }
      seenSeqsByRun.delete(runId);
      set((s) => {
        const { [runId]: _removed, ...channels } = s.channels;
        const channelOrder = s.channelOrder.filter((id) => id !== runId);
        const activeRunId =
          s.activeRunId === runId ? (channelOrder[0] ?? null) : s.activeRunId;
        return { channels, channelOrder, activeRunId };
      });
    },

    applyEvent(runId, ev) {
      const channel = get().channels[runId];
      if (!channel) {
        console.warn("applyEvent: ignoring event for unknown run_id", runId, ev);
        return;
      }
      const updated = applyEventToChannel(channel, ev, seenSeqsFor(runId));
      if (updated === channel) return;
      set((s) => ({ channels: { ...s.channels, [runId]: updated } }));
    },
  }));
}

export const defaultSource: RunEventSource = createDefaultSource();
export const useRunStore = createRunStore(defaultSource);
