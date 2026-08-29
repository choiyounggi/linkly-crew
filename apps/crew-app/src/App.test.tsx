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
