// t8-fe-onboarding: thin invoke adapter for the onboarding/settings backend
// (t3-be-project's ProjectApi/OnboardingStatusApi contract stub, types.ts).
// Deliberately outside `RunEventSource` (lib/source.ts) — that interface is
// scoped to run-event sources; onboarding commands are app-scoped, not
// run-scoped (plan D2).

import { invoke as tauriInvoke, isTauri } from "@tauri-apps/api/core";

import type { AppSettings, ToolStatus } from "./types";

/** `invoke`'s shape, narrowed to what this adapter calls — injectable for tests (mirrors `lib/tauri-source.ts`'s `Invoke`). */
export type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

function defaultInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return tauriInvoke<T>(cmd, args);
}

/** Fixed browser-demo data (plan Task01 step1): no Tauri bridge to call, so `onboarding_status`'s registry-order rows are mirrored here verbatim (decisions.md t3-be-project). */
const MOCK_TOOL_STATUSES: ToolStatus[] = [
  { id: "git", installed: true, path: "/usr/bin/git", version: "git version 2.43.0", install_command: "brew install git" },
  { id: "gh", installed: true, path: "/usr/local/bin/gh", version: "gh version 2.60.0", install_command: "brew install gh", authenticated: true },
  { id: "claude", installed: true, path: "/usr/local/bin/claude", version: "1.0.0", install_command: "npm install -g @anthropic-ai/claude-code" },
  { id: "codex", installed: false, path: null, version: null, install_command: "npm install -g @openai/codex" },
  { id: "opencode", installed: false, path: null, version: null, install_command: "npm install -g opencode-ai" },
  { id: "gemini", installed: false, path: null, version: null, install_command: "npm install -g @google/gemini-cli" },
  { id: "grok", installed: false, path: null, version: null, install_command: "npm install -g @vibe-kit/grok-cli" },
  { id: "pi", installed: false, path: null, version: null, install_command: "npm install -g @mariozechner/pi" },
];

const MOCK_SETTINGS: AppSettings = { workspace_root: "~/linkly-crew/workspace" };

export interface OnboardingApi {
  getSettings(): Promise<AppSettings>;
  setSettings(workspaceRoot: string): Promise<AppSettings>;
  onboardingStatus(): Promise<ToolStatus[]>;
}

/**
 * Creates the onboarding/settings adapter (plan D2). `invoke`/`isTauriFn`
 * are injected (mirrors `tauri-source.ts`'s constructor pattern) so tests
 * never touch a real Tauri bridge. In a plain-browser demo (`isTauriFn()`
 * false, no backend to call) every method resolves fixed mock data instead
 * of invoking.
 */
export function createOnboardingApi(invoke: Invoke = defaultInvoke, isTauriFn: () => boolean = isTauri): OnboardingApi {
  return {
    async getSettings(): Promise<AppSettings> {
      if (!isTauriFn()) return MOCK_SETTINGS;
      return invoke<AppSettings>("get_settings");
    },
    async setSettings(workspaceRoot: string): Promise<AppSettings> {
      if (!isTauriFn()) return { workspace_root: workspaceRoot };
      // Command shape per decisions.md (t3-be-project): set_settings({settings:{workspace_root}}).
      return invoke<AppSettings>("set_settings", { settings: { workspace_root: workspaceRoot } });
    },
    async onboardingStatus(): Promise<ToolStatus[]> {
      if (!isTauriFn()) return MOCK_TOOL_STATUSES;
      return invoke<ToolStatus[]>("onboarding_status");
    },
  };
}

export const defaultOnboardingApi: OnboardingApi = createOnboardingApi();
