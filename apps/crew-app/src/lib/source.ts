import { isTauri } from "@tauri-apps/api/core";

import type { HarnessInfo, Roster, RosterPreset, RunEvent } from "./types";
import { MockEventSource } from "./mock-source";
import { TauriEventSource } from "./tauri-source";

/**
 * Verbatim per contracts-m4.md §C4, extended per contracts-m5.md §C7a.
 * `t-bridge` implements a `TauriEventSource` (`src/lib/tauri-source.ts`)
 * against this same interface — out of scope here. The 5 roster/harness
 * methods are optional so existing sources keep compiling unchanged.
 */
export interface RunEventSource {
  start(goal: string): Promise<void>;
  /** Returns an unsubscribe function. */
  onEvent(cb: (ev: RunEvent) => void): () => void;
  stop(): Promise<void>;
  swapHarness?(agentId: string, harness: string): Promise<void>;
  getRoster?(): Promise<Roster>;
  setRoster?(roster: Roster): Promise<void>;
  listPresets?(): Promise<RosterPreset[]>;
  detectHarnesses?(): Promise<HarnessInfo[]>;
}

/**
 * Detects a Tauri runtime (`@tauri-apps/api/core`'s `isTauri()`, contracts-m4.md
 * §C6 / plan D7) and returns a `TauriEventSource` against it; otherwise (plain
 * browser dev, component tests) a `MockEventSource`.
 */
export function createDefaultSource(): RunEventSource {
  return isTauri() ? new TauriEventSource() : new MockEventSource();
}
