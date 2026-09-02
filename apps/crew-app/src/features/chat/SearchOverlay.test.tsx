import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import SearchOverlay from "./SearchOverlay";
import type { RunEventSource } from "../../lib/source";
import type { Envelope } from "../../lib/types";

function fakeSource(overrides: Partial<RunEventSource> = {}): RunEventSource {
  return {
    start: vi.fn(async () => "run_1"),
    onEvent: vi.fn(() => () => {}),
    stop: vi.fn(async () => {}),
    remove: vi.fn(async () => {}),
    resync: vi.fn(async () => {}),
    listRuns: vi.fn(async () => []),
    createProject: vi.fn(async (name: string) => ({ name, path: `/x/${name}` })),
    searchMessages: vi.fn(async () => []),
    ...overrides,
  };
}

function envelope(overrides: Partial<Envelope> & Pick<Envelope, "id" | "kind" | "thread" | "from">): Envelope {
  return {
    ts: "2026-08-28T00:00:00.000Z",
    sprint: "sprint-1",
    to: ["agent:pm"],
    corr: overrides.thread,
    body: {},
    artifacts: [],
    requires_ack: false,
    deadline_ms: 1000,
    ...overrides,
  };
}

describe("SearchOverlay — normal query + result click (D9)", () => {
  it("calls searchMessages(runId, query) and renders results", async () => {
    const searchMessages = vi.fn(async () => [
      { seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-pm", from: "lead" }) },
    ]);
    render(<SearchOverlay runId="run_1" onClose={() => {}} onOpenThread={() => {}} source={fakeSource({ searchMessages })} />);

    fireEvent.change(screen.getByLabelText("메시지 검색"), { target: { value: "hello" } });

    await waitFor(() => expect(searchMessages).toHaveBeenCalledWith("run_1", "hello"));
    expect(await screen.findByText("lead")).toBeInTheDocument();
  });

  it("calls onOpenThread with the result's thread id when a result is clicked", async () => {
    let opened: string | null = null;
    const searchMessages = vi.fn(async () => [
      { seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-pm", from: "lead" }) },
    ]);
    render(
      <SearchOverlay
        runId="run_1"
        onClose={() => {}}
        onOpenThread={(threadId) => (opened = threadId)}
        source={fakeSource({ searchMessages })}
      />,
    );

    fireEvent.change(screen.getByLabelText("메시지 검색"), { target: { value: "hello" } });
    fireEvent.click(await screen.findByText("lead"));

    expect(opened).toBe("t-pm");
  });
});

describe("SearchOverlay — race guard: only the latest query's result is applied (D9)", () => {
  it("discards a slow earlier response that resolves after a faster later query", async () => {
    let resolveFirst!: (v: { seq: number; envelope: Envelope }[]) => void;
    const first = new Promise<{ seq: number; envelope: Envelope }[]>((resolve) => {
      resolveFirst = resolve;
    });
    const searchMessages = vi.fn((_runId: string, q: string) => {
      if (q === "first") return first;
      return Promise.resolve([{ seq: 2, envelope: envelope({ id: "e2", kind: "task.assign", thread: "t-second", from: "second-result" }) }]);
    });

    render(<SearchOverlay runId="run_1" onClose={() => {}} onOpenThread={() => {}} source={fakeSource({ searchMessages })} />);
    const input = screen.getByLabelText("메시지 검색");

    fireEvent.change(input, { target: { value: "first" } }); // slow, resolves later
    fireEvent.change(input, { target: { value: "second" } }); // fast, resolves first

    await screen.findByText("second-result");

    // now let the stale "first" response resolve — it must NOT overwrite "second"'s result
    resolveFirst([{ seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-first", from: "first-result" }) }]);
    await Promise.resolve();
    await Promise.resolve();

    expect(screen.queryByText("first-result")).not.toBeInTheDocument();
    expect(screen.getByText("second-result")).toBeInTheDocument();
  });
});

describe("SearchOverlay — error surfacing (error) and empty query (boundary)", () => {
  it("shows the thrown error message", async () => {
    const searchMessages = vi.fn(async () => {
      throw new Error("search_failed");
    });
    render(<SearchOverlay runId="run_1" onClose={() => {}} onOpenThread={() => {}} source={fakeSource({ searchMessages })} />);

    fireEvent.change(screen.getByLabelText("메시지 검색"), { target: { value: "boom" } });

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("search_failed"));
  });

  it("clears results and does not call searchMessages for an empty query (boundary)", async () => {
    const searchMessages = vi.fn(async () => []);
    render(<SearchOverlay runId="run_1" onClose={() => {}} onOpenThread={() => {}} source={fakeSource({ searchMessages })} />);
    const input = screen.getByLabelText("메시지 검색");

    fireEvent.change(input, { target: { value: "x" } });
    fireEvent.change(input, { target: { value: "" } });

    expect(searchMessages).not.toHaveBeenCalledWith("run_1", "");
  });
});
