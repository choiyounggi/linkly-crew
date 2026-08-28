import type { RunEvent } from "./types";
import { MockEventSource } from "./mock-source";

/**
 * Verbatim per contracts-m4.md §C4. `t-bridge` implements a
 * `TauriEventSource` (`src/lib/tauri-source.ts`) against this same
 * interface — out of scope here.
 */
export interface RunEventSource {
  start(goal: string): Promise<void>;
  /** Returns an unsubscribe function. */
  onEvent(cb: (ev: RunEvent) => void): () => void;
  stop(): Promise<void>;
}

/**
 * Always returns a MockEventSource today. `t-bridge` owns swapping this to
 * detect a Tauri runtime and return `TauriEventSource` instead (plan step 4).
 */
export function createDefaultSource(): RunEventSource {
  return new MockEventSource();
}
