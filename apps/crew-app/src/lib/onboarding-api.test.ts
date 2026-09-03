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

  // -- normal: folder picker returns the chosen path -----------------------

  it("resolves the picked directory and passes an absolute current path as defaultPath", async () => {
    const openDialog = vi.fn(async () => "/home/dev/picked-workspace");
    const api = createOnboardingApi(vi.fn() as Invoke, ALWAYS_TAURI, openDialog);

    await expect(api.pickWorkspaceDirectory!("/home/dev/workspace")).resolves.toBe("/home/dev/picked-workspace");
    expect(openDialog).toHaveBeenCalledWith({ directory: true, multiple: false, defaultPath: "/home/dev/workspace" });
  });

  // -- boundary: cancel resolves null, not an error -------------------------

  it("resolves null when the user cancels the picker", async () => {
    const openDialog = vi.fn(async () => null);
    const api = createOnboardingApi(vi.fn() as Invoke, ALWAYS_TAURI, openDialog);

    await expect(api.pickWorkspaceDirectory!("/home/dev/workspace")).resolves.toBeNull();
  });

  // -- boundary: a tilde/empty current path must not become defaultPath -----

  it("omits defaultPath when the current value is not an absolute path (the OS dialog does not expand ~)", async () => {
    const openDialog = vi.fn(async () => "/home/dev/picked");
    const api = createOnboardingApi(vi.fn() as Invoke, ALWAYS_TAURI, openDialog);

    await api.pickWorkspaceDirectory!("~/linkly-crew/workspace");
    expect(openDialog).toHaveBeenCalledWith({ directory: true, multiple: false });

    await api.pickWorkspaceDirectory!("");
    expect(openDialog).toHaveBeenLastCalledWith({ directory: true, multiple: false });

    await api.pickWorkspaceDirectory!();
    expect(openDialog).toHaveBeenLastCalledWith({ directory: true, multiple: false });
  });

  // -- boundary: the union return type is narrowed, not asserted ------------

  it("narrows an array result to its first entry and an empty array to null", async () => {
    const arrayOpen = vi.fn(async () => ["/home/dev/first", "/home/dev/second"]);
    await expect(createOnboardingApi(vi.fn() as Invoke, ALWAYS_TAURI, arrayOpen).pickWorkspaceDirectory!()).resolves.toBe("/home/dev/first");

    const emptyOpen = vi.fn(async () => [] as string[]);
    await expect(createOnboardingApi(vi.fn() as Invoke, ALWAYS_TAURI, emptyOpen).pickWorkspaceDirectory!()).resolves.toBeNull();
  });

  // -- boundary: no native dialog outside Tauri -----------------------------

  it("omits pickWorkspaceDirectory entirely outside Tauri so the UI hides 찾아보기", async () => {
    const openDialog = vi.fn(async () => "/should/not/be/reached");
    const api = createOnboardingApi(vi.fn() as Invoke, NEVER_TAURI, openDialog);

    // The key must be ABSENT, not a method that resolves null: OnboardingPanels
    // renders the button on `typeof api.pickWorkspaceDirectory === "function"`,
    // so an inert method would produce a button that does nothing when clicked.
    expect(api.pickWorkspaceDirectory).toBeUndefined();
    expect("pickWorkspaceDirectory" in api).toBe(false);
    expect(openDialog).not.toHaveBeenCalled();
  });

  it("defines pickWorkspaceDirectory under Tauri", async () => {
    const api = createOnboardingApi(vi.fn() as Invoke, ALWAYS_TAURI, vi.fn(async () => null));
    expect(typeof api.pickWorkspaceDirectory).toBe("function");
  });

  // -- error: a dialog rejection propagates ---------------------------------

  it("propagates a picker rejection instead of swallowing it into a silent cancel", async () => {
    const openDialog = vi.fn(async () => {
      throw new Error("dialog.open not allowed");
    });
    const api = createOnboardingApi(vi.fn() as Invoke, ALWAYS_TAURI, openDialog);

    await expect(api.pickWorkspaceDirectory!()).rejects.toThrow("dialog.open not allowed");
  });
});
