import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { emptyChannel, useRunStore } from "../../lib/store";
import Sidebar from "./Sidebar";

afterEach(() => {
  useRunStore.setState({ activeRunId: null, channelOrder: [], channels: {} });
});

describe("Sidebar — normal", () => {
  it("lists channels with their goal label and status badge, selecting one on click", () => {
    act(() => {
      useRunStore.setState({
        activeRunId: "run-1",
        channelOrder: ["run-1", "run-2"],
        channels: {
          "run-1": { ...emptyChannel("run-1", "랜딩 페이지", false), finished: null },
          "run-2": { ...emptyChannel("run-2", "완료된 작업", false), finished: "completed" },
        },
      });
    });

    render(<Sidebar onNewTask={() => {}} onOpenSettings={() => {}} />);

    expect(screen.getByRole("navigation", { name: "채널 목록" })).toBeInTheDocument();
    expect(screen.getByText("랜딩 페이지")).toBeInTheDocument();
    expect(screen.getByText("완료된 작업")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "랜딩 페이지 진행 중" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "완료된 작업 완료" })).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByText("완료")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "완료된 작업 완료" }));
    expect(useRunStore.getState().activeRunId).toBe("run-2");
  });

  it("calls onNewTask/onOpenSettings from the + and settings buttons", () => {
    const onNewTask = vi.fn();
    const onOpenSettings = vi.fn();
    render(<Sidebar onNewTask={onNewTask} onOpenSettings={onOpenSettings} />);

    fireEvent.click(screen.getByRole("button", { name: "새 작업" }));
    fireEvent.click(screen.getByRole("button", { name: "설정" }));

    expect(onNewTask).toHaveBeenCalledTimes(1);
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
  });
});

describe("Sidebar — boundary: no channels", () => {
  it("shows an empty-list message and renders no channel buttons", () => {
    render(<Sidebar onNewTask={() => {}} onOpenSettings={() => {}} />);

    expect(screen.getByText("진행 중인 채널이 없습니다")).toBeInTheDocument();
    expect(screen.queryAllByRole("button", { name: /채널 닫기/ })).toHaveLength(0);
  });
});

describe("Sidebar — error/edge: closing a channel", () => {
  it("calls closeChannel for that run_id without also selecting it", () => {
    act(() => {
      useRunStore.setState({
        activeRunId: "run-1",
        channelOrder: ["run-1"],
        channels: { "run-1": emptyChannel("run-1", "goal", false) },
      });
    });
    const closeChannel = vi.spyOn(useRunStore.getState(), "closeChannel").mockResolvedValue(undefined);

    render(<Sidebar onNewTask={() => {}} onOpenSettings={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /채널 닫기/ }));

    expect(closeChannel).toHaveBeenCalledWith("run-1");
    closeChannel.mockRestore();
  });
});
