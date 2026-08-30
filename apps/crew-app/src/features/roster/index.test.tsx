import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useRunStore } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import type { HarnessInfo, Roster, RosterPreset } from "../../lib/types";
import RosterPanel from "./index";

function createFakeSource(overrides: Partial<RunEventSource> = {}): RunEventSource {
  return {
    start: vi.fn(async () => {}),
    onEvent: vi.fn(() => () => {}),
    stop: vi.fn(async () => {}),
    ...overrides,
  };
}

const SAMPLE_ROSTER: Roster = {
  agents: [
    { id: "agent:designer", role: "designer", harness: "claude-code", model: "claude-sonnet-5", instructions: "" },
    { id: "agent:developer", role: "developer", harness: "claude-code", model: "claude-sonnet-5", instructions: "" },
  ],
};

const SAMPLE_PRESET: RosterPreset = {
  name: "절약 모드",
  roster: {
    agents: [
      { id: "agent:designer", role: "designer", harness: "opencode", model: "default", instructions: "" },
      { id: "agent:developer", role: "developer", harness: "claude-code", model: "claude-opus-5", instructions: "" },
    ],
  },
};

const SAMPLE_HARNESSES: HarnessInfo[] = [
  { id: "claude-code", installed: true, path: "/usr/local/bin/claude", adapter: "real" },
  { id: "opencode", installed: true, path: "/usr/local/bin/opencode", adapter: "stub" },
  { id: "codex", installed: false, path: null, adapter: "none" },
];

afterEach(() => {
  useRunStore.setState({ runId: null, finished: null, roster: [] });
});

describe("RosterPanel", () => {
  it("loads presets/harnesses/roster, applies a selected preset to the draft, and saves it", async () => {
    const setRoster = vi.fn(async () => {});
    const source = createFakeSource({
      listPresets: vi.fn(async () => [SAMPLE_PRESET]),
      detectHarnesses: vi.fn(async () => SAMPLE_HARNESSES),
      getRoster: vi.fn(async () => SAMPLE_ROSTER),
      setRoster,
    });

    render(<RosterPanel source={source} />);

    await screen.findByRole("option", { name: "절약 모드" });
    expect(screen.getByLabelText("designer 모델")).toHaveValue("claude-sonnet-5");

    const designerHarness = screen.getByLabelText("designer 하네스");
    expect(within(designerHarness).getByRole("option", { name: "codex" })).toBeDisabled();
    expect(within(designerHarness).getByRole("option", { name: "claude-code" })).toBeEnabled();

    fireEvent.change(screen.getByLabelText("프리셋"), { target: { value: "절약 모드" } });

    expect(screen.getByLabelText("designer 모델")).toHaveValue("default");
    expect(screen.getByLabelText("developer 모델")).toHaveValue("claude-opus-5");

    fireEvent.click(screen.getByRole("button", { name: "저장" }));

    await waitFor(() => expect(setRoster).toHaveBeenCalledWith(SAMPLE_PRESET.roster));
    expect(await screen.findByText("저장됨")).toBeInTheDocument();
  });

  it("enables the swap button only while a run is active, and calls swapHarness with the slot's selected harness", async () => {
    const swapHarness = vi.fn(async () => {});
    const source = createFakeSource({
      listPresets: vi.fn(async () => []),
      detectHarnesses: vi.fn(async () => SAMPLE_HARNESSES),
      getRoster: vi.fn(async () => SAMPLE_ROSTER),
      setRoster: vi.fn(async () => {}),
      swapHarness,
    });

    render(<RosterPanel source={source} />);

    const swapButton = await screen.findByRole("button", { name: "designer 교체" });
    expect(swapButton).toBeDisabled();

    act(() => {
      useRunStore.getState().applyEvent({ type: "run_started", run_id: "run-1", goal: "g", ts: "" });
    });

    expect(swapButton).toBeEnabled();

    fireEvent.change(screen.getByLabelText("designer 하네스"), { target: { value: "opencode" } });
    fireEvent.click(swapButton);

    await waitFor(() => expect(swapHarness).toHaveBeenCalledWith("agent:designer", "opencode"));

    act(() => {
      useRunStore.getState().applyEvent({ type: "run_finished", outcome: "completed", ts: "" });
    });

    expect(swapButton).toBeDisabled();
  });

  it("shows an inline error when saving the roster fails", async () => {
    const setRoster = vi.fn(async () => {
      throw new Error("save boom");
    });
    const source = createFakeSource({
      listPresets: vi.fn(async () => []),
      detectHarnesses: vi.fn(async () => []),
      getRoster: vi.fn(async () => SAMPLE_ROSTER),
      setRoster,
    });

    render(<RosterPanel source={source} />);
    await screen.findByLabelText("designer 모델");

    fireEvent.click(screen.getByRole("button", { name: "저장" }));

    expect(await screen.findByText(/save boom/)).toBeInTheDocument();
  });

  it("renders an empty slot list without crashing when the roster has no agents", async () => {
    const source = createFakeSource({
      listPresets: vi.fn(async () => []),
      detectHarnesses: vi.fn(async () => []),
      getRoster: vi.fn(async () => ({ agents: [] })),
      setRoster: vi.fn(async () => {}),
    });

    render(<RosterPanel source={source} />);

    expect(await screen.findByText("슬롯 없음")).toBeInTheDocument();
  });

  it("shows an unsupported-source fallback and does not crash when no optional roster methods exist", () => {
    const source = createFakeSource();

    render(<RosterPanel source={source} />);

    expect(screen.getByText("이 소스에서 지원 안 함")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "저장" })).not.toBeInTheDocument();
  });

  const FOUR_ROLE_ROSTER: Roster = {
    agents: [
      { id: "agent:pm", role: "pm", harness: "claude-code", model: "default", instructions: "" },
      { id: "agent:designer", role: "designer", harness: "claude-code", model: "default", instructions: "" },
      { id: "agent:publisher", role: "publisher", harness: "claude-code", model: "default", instructions: "" },
      { id: "agent:developer", role: "developer", harness: "claude-code", model: "default", instructions: "" },
    ],
  };

  const LEAD_ROSTER: Roster = {
    agents: [
      { id: "agent:lead", role: "lead", harness: "claude-code", model: "default", instructions: "" },
      { id: "agent:designer", role: "designer", harness: "claude-code", model: "default", instructions: "" },
    ],
  };

  const FULL_ROSTER: Roster = {
    agents: [
      { id: "agent:pm", role: "pm", harness: "claude-code", model: "default", instructions: "" },
      { id: "agent:designer", role: "designer", harness: "claude-code", model: "default", instructions: "" },
      { id: "agent:publisher", role: "publisher", harness: "claude-code", model: "default", instructions: "" },
      { id: "agent:developer", role: "developer", harness: "claude-code", model: "default", instructions: "" },
      { id: "agent:qa", role: "qa", harness: "claude-code", model: "default", instructions: "" },
    ],
  };

  it("adds a slot for the selected unassigned role with contract-default fields", async () => {
    const setRoster = vi.fn(async () => {});
    const source = createFakeSource({
      listPresets: vi.fn(async () => []),
      detectHarnesses: vi.fn(async () => []),
      getRoster: vi.fn(async () => FOUR_ROLE_ROSTER),
      setRoster,
    });

    render(<RosterPanel source={source} />);
    await screen.findByLabelText("pm 모델");

    const addSelect = screen.getByLabelText("추가할 역할");
    expect(within(addSelect).getByRole("option", { name: "qa" })).toBeInTheDocument();

    fireEvent.change(addSelect, { target: { value: "qa" } });
    fireEvent.click(screen.getByRole("button", { name: "추가" }));

    expect(screen.getByLabelText("qa 모델")).toHaveValue("default");

    fireEvent.click(screen.getByRole("button", { name: "저장" }));

    await waitFor(() =>
      expect(setRoster).toHaveBeenCalledWith({
        agents: [
          ...FOUR_ROLE_ROSTER.agents,
          { id: "agent:qa", role: "qa", harness: "claude-code", model: "default", instructions: "" },
        ],
      }),
    );
  });

  it("disables deleting the lead slot but removes a non-lead slot on click", async () => {
    const source = createFakeSource({
      listPresets: vi.fn(async () => []),
      detectHarnesses: vi.fn(async () => []),
      getRoster: vi.fn(async () => LEAD_ROSTER),
      setRoster: vi.fn(async () => {}),
    });

    render(<RosterPanel source={source} />);
    await screen.findByLabelText("designer 모델");

    expect(screen.getByRole("button", { name: "슬롯 삭제 agent:lead" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "슬롯 삭제 agent:designer" }));

    expect(screen.queryByLabelText("designer 모델")).not.toBeInTheDocument();
  });

  it("disables the add-role select and button once all five roles are assigned", async () => {
    const source = createFakeSource({
      listPresets: vi.fn(async () => []),
      detectHarnesses: vi.fn(async () => []),
      getRoster: vi.fn(async () => FULL_ROSTER),
      setRoster: vi.fn(async () => {}),
    });

    render(<RosterPanel source={source} />);
    await screen.findByLabelText("qa 모델");

    expect(screen.getByLabelText("추가할 역할")).toBeDisabled();
    expect(screen.getByRole("button", { name: "추가" })).toBeDisabled();
  });
});
