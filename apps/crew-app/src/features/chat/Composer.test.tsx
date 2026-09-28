import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import Composer from "./Composer";
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

describe("Composer — no resolveGate on the source (boundary, D3d — REPLACES the old 'no active gate' disabled-state describe block, since activeGate is now required and that state can no longer be constructed)", () => {
  it("renders nothing when the source has no resolveGate method", () => {
    const { container } = render(
      <Composer runId="run_1" activeGate={{ taskId: "t-qa", reason: "why" }} source={fakeSource({ resolveGate: undefined })} />,
    );

    expect(container.firstChild).toBeNull();
  });
});

describe("Composer — active gate (normal, D6)", () => {
  it("is enabled and calls resolveGate with the typed reason on approve", async () => {
    const resolveGate = vi.fn(async () => {});
    render(<Composer runId="run_1" activeGate={{ taskId: "t-qa", reason: "why" }} source={fakeSource({ resolveGate })} />);

    const input = screen.getByLabelText("게이트 응답");
    expect(input).not.toBeDisabled();
    fireEvent.change(input, { target: { value: "확인했습니다" } });
    fireEvent.click(screen.getByRole("button", { name: "승인" }));

    await waitFor(() => expect(resolveGate).toHaveBeenCalledWith("run_1", "t-qa", "approve", "확인했습니다"));
  });

  it("surfaces a thrown error instead of failing silently (error)", async () => {
    const resolveGate = vi.fn(async () => {
      throw new Error("run_not_found");
    });
    render(<Composer runId="run_1" activeGate={{ taskId: "t-qa", reason: "why" }} source={fakeSource({ resolveGate })} />);

    fireEvent.click(screen.getByRole("button", { name: "반려" }));

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("run_not_found"));
  });
});
