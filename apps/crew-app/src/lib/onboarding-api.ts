// t8-fe-onboarding: thin invoke adapter for the onboarding/settings backend
// (t3-be-project's ProjectApi/OnboardingStatusApi contract stub, types.ts).
// Deliberately outside `RunEventSource` (lib/source.ts) — that interface is
// scoped to run-event sources; onboarding commands are app-scoped, not
// run-scoped (plan D2).

import { invoke as tauriInvoke, isTauri } from "@tauri-apps/api/core";
import { open as tauriOpen } from "@tauri-apps/plugin-dialog";

import type { AppSettings, ToolStatus } from "./types";

/** `invoke`'s shape, narrowed to what this adapter calls — injectable for tests (mirrors `lib/tauri-source.ts`'s `Invoke`). */
export type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * `@tauri-apps/plugin-dialog`'s `open`, narrowed to the directory-picker call
 * this adapter makes — injectable for tests, which must never open a real
 * native dialog (it would block the run waiting for a human).
 *
 * The plugin resolves `null` when the user cancels. With `multiple` unset it
 * resolves a single path, but the published type is a union, so the adapter
 * narrows it rather than asserting.
 */
export type OpenDialog = (options: { directory: true; multiple: false; defaultPath?: string }) => Promise<string | string[] | null>;

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
  /**
   * Opens the OS folder picker and resolves the chosen absolute path, or
   * `null` if the user cancelled. Picking only fills the input — saving stays
   * an explicit press of 저장, same as typing the path by hand.
   *
   * Optional, and genuinely **absent** outside Tauri — `createOnboardingApi`
   * omits the key rather than defining a method that returns `null`, because
   * the UI's `typeof api.pickWorkspaceDirectory === "function"` check is what
   * decides whether to render 찾아보기 at all. A plain browser has no native
   * folder picker, so the button must not exist there; a present-but-inert
   * method would render one that silently does nothing when clicked.
   */
  pickWorkspaceDirectory?(currentPath?: string): Promise<string | null>;
}

/**
 * Creates the onboarding/settings adapter (plan D2). `invoke`/`isTauriFn`
 * are injected (mirrors `tauri-source.ts`'s constructor pattern) so tests
 * never touch a real Tauri bridge. In a plain-browser demo (`isTauriFn()`
 * false, no backend to call) every method resolves fixed mock data instead
 * of invoking.
 */
export function createOnboardingApi(
  invoke: Invoke = defaultInvoke,
  isTauriFn: () => boolean = isTauri,
  openDialog: OpenDialog = tauriOpen as OpenDialog,
): OnboardingApi {
  const api: OnboardingApi = {
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

  // The key itself is conditional, NOT a guard inside the method body: the UI
  // decides whether to render 찾아보기 with `typeof api.pickWorkspaceDirectory
  // === "function"`, so a method that exists and resolves null outside Tauri
  // would render a button that silently does nothing. Unlike the methods
  // above, `isTauri()` is a fixed property of the environment (the bridge is
  // installed before app code runs), so deciding once at construction is
  // sound.
  if (isTauriFn()) {
    api.pickWorkspaceDirectory = async (currentPath?: string): Promise<string | null> => {
      // `defaultPath` only when the current value is already an absolute path:
      // the stored default is `~/linkly-crew/workspace`, and the OS dialog does
      // not expand `~` — handing it an unexpanded tilde makes it fall back to
      // an arbitrary directory instead of the one shown in the field.
      const defaultPath = currentPath?.startsWith("/") ? currentPath : undefined;
      const picked = await openDialog({ directory: true, multiple: false, ...(defaultPath ? { defaultPath } : {}) });
      // Cancel resolves null. `multiple: false` resolves a single path, but the
      // plugin's published type is a union — narrow it rather than assert.
      if (picked === null) return null;
      return Array.isArray(picked) ? (picked[0] ?? null) : picked;
    };
  }

  return api;
}

export const defaultOnboardingApi: OnboardingApi = createOnboardingApi();
