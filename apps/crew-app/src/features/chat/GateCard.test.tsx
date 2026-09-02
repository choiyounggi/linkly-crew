import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import GateCard from "./GateCard";
import type { RunEventSource } from "../../lib/source";

function fakeSource(overrides: Partial<RunEventSource> = {}): RunEventSource {
  return {
    start: vi.fn(async () => "run_1"),
    onEvent: vi.fn(() => () => {}),
    stop: vi.fn(async () => {}),
    remove: vi.fn(async () => {}),
    resync: vi.fn(async () => {}),
    listRuns: vi.fn(async () => []),
    createProject: vi.fn(async (name: string) => ({ name, path: `/x/${name}` })),
    resolveGate: vi.fn(async () => {}),
    ...overrides,
  };
}

describe("GateCard — approve/reject call resolveGate(runId, ...) (normal, D3)", () => {
  it("calls resolveGate with the run id, task id, decision, and the gate's reason on approve", async () => {
    const resolveGate = vi.fn(async () => {});
    render(
      <GateCard runId="run_1" taskId="t-qa" reason="검토 필요" resolution={null} source={fakeSource({ resolveGate })} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "승인" }));

    await waitFor(() => expect(resolveGate).toHaveBeenCalledWith("run_1", "t-qa", "approve", "검토 필요"));
  });

  it("calls resolveGate with decision=reject on reject", async () => {
    const resolveGate = vi.fn(async () => {});
    render(
      <GateCard runId="run_1" taskId="t-qa" reason="검토 필요" resolution={null} source={fakeSource({ resolveGate })} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "반려" }));

    await waitFor(() => expect(resolveGate).toHaveBeenCalledWith("run_1", "t-qa", "reject", "검토 필요"));
  });
});

describe("GateCard — error surfacing (error, D3)", () => {
  it("shows the thrown error message and re-enables the buttons for retry", async () => {
    const resolveGate = vi.fn(async () => {
      throw new Error("run_not_found");
    });
    render(
      <GateCard runId="run_1" taskId="t-qa" reason="검토 필요" resolution={null} source={fakeSource({ resolveGate })} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "승인" }));

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("run_not_found"));
    expect(screen.getByRole("button", { name: "승인" })).not.toBeDisabled();
  });
});

describe("GateCard — disabled once resolved (boundary, D3)", () => {
  it("hides the action buttons and shows the outcome once a resolution is supplied", () => {
    render(
      <GateCard
        runId="run_1"
        taskId="t-qa"
        reason="검토 필요"
        resolution={{ task_id: "t-qa", decision: "approve", reason: "ok" }}
        source={fakeSource()}
      />,
    );

    expect(screen.queryByRole("button", { name: "승인" })).not.toBeInTheDocument();
    expect(screen.getByText(/승인됨/)).toBeInTheDocument();
  });

  it("does not call resolveGate when the source has no resolveGate method (boundary)", () => {
    const source = fakeSource();
    delete source.resolveGate;
    render(<GateCard runId="run_1" taskId="t-qa" reason="검토 필요" resolution={null} source={source} />);

    expect(screen.getByRole("button", { name: "승인" })).toBeDisabled();
  });
});
