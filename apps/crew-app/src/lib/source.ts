import { isTauri } from "@tauri-apps/api/core";

import type { RunEvent } from "./types";
import { MockEventSource } from "./mock-source";
import { TauriEventSource } from "./tauri-source";

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
 * Detects a Tauri runtime (`@tauri-apps/api/core`'s `isTauri()`, contracts-m4.md
 * §C6 / plan D7) and returns a `TauriEventSource` against it; otherwise (plain
 * browser dev, component tests) a `MockEventSource`.
 */
export function createDefaultSource(): RunEventSource {
  return isTauri() ? new TauriEventSource() : new MockEventSource();
}
