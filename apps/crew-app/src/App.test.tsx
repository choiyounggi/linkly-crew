import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import App from "./App";
import { emptyChannel, useRunStore } from "./lib/store";

const ONBOARDED_KEY = "crew.onboarded";

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  localStorage.clear();
  useRunStore.setState({ activeRunId: null, channelOrder: [], channels: {} });
});

describe("App — onboarding gate (plan D7)", () => {
  it("shows OnboardingFlow when the crew.onboarded flag is absent", () => {
    render(<App />);
    expect(screen.getByLabelText("온보딩")).toBeInTheDocument();
    expect(screen.queryByLabelText("채널 목록")).not.toBeInTheDocument();
  });

  it("shows the shell directly when the flag is already set", () => {
    localStorage.setItem(ONBOARDED_KEY, "1");
    render(<App />);
    expect(screen.getByLabelText("채널 목록")).toBeInTheDocument();
    expect(screen.queryByLabelText("온보딩")).not.toBeInTheDocument();
  });
});

describe("App — shell shape (plan D5/D6/R5/R6/R8)", () => {
  beforeEach(() => {
    localStorage.setItem(ONBOARDED_KEY, "1");
  });

  it("has no leftover view tabs / command-bar goal input from the old single-run UI", () => {
    render(<App />);
    expect(screen.queryByLabelText("요청")).not.toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "뷰 전환" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "시작" })).not.toBeInTheDocument();
  });

  it("shows the empty-channel state with a way to start a new task when there are no channels", () => {
    render(<App />);
    expect(screen.getByText("아직 채널이 없습니다")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "새 작업 시작" })).toBeInTheDocument();
  });

  it("mounts ChatPane for the active channel once one exists", () => {
    act(() => {
      useRunStore.setState({
        activeRunId: "run-1",
        channelOrder: ["run-1"],
        channels: { "run-1": emptyChannel("run-1", "랜딩 페이지", false) },
      });
    });

    render(<App />);

    expect(screen.getByLabelText("채널 대화")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "랜딩 페이지" })).toBeInTheDocument();
  });
});

describe("App — sidebar new-task / settings routing", () => {
  beforeEach(() => {
    localStorage.setItem(ONBOARDED_KEY, "1");
  });

  it("opens the new-task modal from the sidebar's + button", () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "새 작업" }));
    expect(screen.getByRole("dialog", { name: "새 작업" })).toBeInTheDocument();
  });

  it("opens SettingsMenu from the sidebar's settings button", () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "설정" }));
    // SettingsMenu's <section aria-label="설정"> exposes accessible role
    // "region" — distinct from the sidebar's still-mounted "설정" button.
    expect(screen.getByRole("region", { name: "설정" })).toBeInTheDocument();
  });
});
