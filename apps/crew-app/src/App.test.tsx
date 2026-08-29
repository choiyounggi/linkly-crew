import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import App from "./App";
import { useRunStore } from "./lib/store";

describe("App", () => {
  it("renders the command bar and all four panel stubs", () => {
    render(<App />);
    expect(screen.getByLabelText("요청")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "시작" })).toBeInTheDocument();
    expect(screen.getByLabelText("에이전트 레일")).toBeInTheDocument();
    expect(screen.getByLabelText("스프린트 보드")).toBeInTheDocument();
    expect(screen.getByLabelText("라이브 스레드")).toBeInTheDocument();
    expect(screen.getByLabelText("로스터")).toBeInTheDocument();
  });

  it("shows a validation error and starts no run for an empty/whitespace-only goal", () => {
    render(<App />);
    const input = screen.getByLabelText("요청");
    const button = screen.getByRole("button", { name: "시작" });

    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.click(button);

    expect(screen.getByText("요청을 입력하세요")).toBeInTheDocument();
    expect(useRunStore.getState().runId).toBeNull();
  });

  it("does not disable the start button before any run has started", () => {
    render(<App />);
    expect(screen.getByRole("button", { name: "시작" })).not.toBeDisabled();
  });
});

describe("App — tab switching (plan D1/D2)", () => {
  it("shows the board by default", () => {
    render(<App />);
    expect(screen.getByLabelText("스프린트 보드")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "보드" })).toHaveAttribute("aria-pressed", "true");
  });

  it("switches to the DAG view when the DAG tab is clicked", () => {
    const { container } = render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "DAG" }));
    expect(container.querySelector("section.dag-view")).toBeInTheDocument();
    expect(screen.queryByLabelText("스프린트 보드")).not.toBeInTheDocument();
  });

  it("switches to the timeline view when the timeline tab is clicked", () => {
    const { container } = render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "타임라인" }));
    expect(container.querySelector("section.timeline-view")).toBeInTheDocument();
    expect(screen.queryByLabelText("스프린트 보드")).not.toBeInTheDocument();
  });
});
