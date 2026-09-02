import { describe, expect, it, vi } from "vitest";

import { createOnboardingApi } from "./onboarding-api";
import type { Invoke } from "./onboarding-api";
import type { ToolStatus } from "./types";

const ALWAYS_TAURI = () => true;
const NEVER_TAURI = () => false;

describe("onboarding-api", () => {
  // -- normal: Tauri round trip -------------------------------------------

  it("round-trips getSettings/setSettings/onboardingStatus through the injected invoke with the exact command shapes", async () => {
    const sampleStatuses: ToolStatus[] = [
      { id: "git", installed: true, path: "/usr/bin/git", version: "git version 2.43.0", install_command: "brew install git" },
    ];
    const invoke: Invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_settings") return { workspace_root: "/home/dev/workspace" };
      if (cmd === "set_settings") return { workspace_root: (args!.settings as { workspace_root: string }).workspace_root };
      if (cmd === "onboarding_status") return sampleStatuses;
      throw new Error(`unexpected cmd ${cmd}`);
    }) as Invoke;
    const api = createOnboardingApi(invoke, ALWAYS_TAURI);

    await expect(api.getSettings()).resolves.toEqual({ workspace_root: "/home/dev/workspace" });
    expect(invoke).toHaveBeenCalledWith("get_settings");

    await expect(api.setSettings("/home/dev/new-workspace")).resolves.toEqual({ workspace_root: "/home/dev/new-workspace" });
    expect(invoke).toHaveBeenCalledWith("set_settings", { settings: { workspace_root: "/home/dev/new-workspace" } });

    await expect(api.onboardingStatus()).resolves.toEqual(sampleStatuses);
    expect(invoke).toHaveBeenCalledWith("onboarding_status");
  });

  // -- error: setSettings rejection propagates -----------------------------

  it("propagates a setSettings rejection (relative-path Err) instead of swallowing it", async () => {
    const invoke: Invoke = vi.fn(async (cmd: string) => {
      if (cmd === "set_settings") throw new Error('workspace_root must be an absolute path, got "relative/workspace"');
      throw new Error(`unexpected cmd ${cmd}`);
    }) as Invoke;
    const api = createOnboardingApi(invoke, ALWAYS_TAURI);

    await expect(api.setSettings("relative/workspace")).rejects.toThrow(/absolute/);
  });

  // -- boundary: non-Tauri (browser demo) fallback never touches invoke ----

  it("falls back to fixed mock data and never calls invoke when not running under Tauri", async () => {
    const invoke: Invoke = vi.fn();
    const api = createOnboardingApi(invoke, NEVER_TAURI);

    const settings = await api.getSettings();
    expect(settings.workspace_root).toBeTruthy();

    const saved = await api.setSettings("/anything/goes");
    expect(saved).toEqual({ workspace_root: "/anything/goes" });

    const statuses = await api.onboardingStatus();
    expect(statuses).toHaveLength(8);
    expect(statuses.map((s) => s.id)).toEqual(["git", "gh", "claude", "codex", "opencode", "gemini", "grok", "pi"]);

    expect(invoke).not.toHaveBeenCalled();
  });
});
