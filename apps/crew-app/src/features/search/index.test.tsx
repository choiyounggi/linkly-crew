import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import SearchBox from "./index";
import { useRunStore } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import type { Envelope } from "../../lib/types";

function createFakeSource(overrides: Partial<RunEventSource> = {}): RunEventSource {
  return {
    start: vi.fn(async () => {}),
    onEvent: vi.fn(() => () => {}),
    stop: vi.fn(async () => {}),
    ...overrides,
  };
}

function envelope(overrides: Partial<Envelope> & Pick<Envelope, "id" | "kind" | "from" | "corr">): Envelope {
  return {
    ts: "2026-08-30T00:00:00.000Z",
    sprint: "sprint-1",
    thread: "t-pm",
    to: ["x"],
    in_reply_to: undefined,
    body: {},
    artifacts: [],
    requires_ack: false,
    deadline_ms: 1000,
    ...overrides,
  };
}

afterEach(() => {
  useRunStore.setState({ messages: [] });
});

describe("SearchBox — normal", () => {
  it("keeps the contract root class and does not call the source for an empty query", async () => {
    const searchMessages = vi.fn(async () => []);
    const source = createFakeSource({ searchMessages });

    const { container } = render(<SearchBox source={source} />);

    expect(container.querySelector("div.search-box")).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "검색" }));

    expect(searchMessages).not.toHaveBeenCalled();
    expect(screen.queryByRole("listbox", { name: "검색 결과" })).toBeNull();
  });

  it("calls source.searchMessages with the trimmed query on submit and renders the results", async () => {
    const found = envelope({ id: "e1", kind: "task.result", from: "agent:pm", corr: "t-pm", body: "hello world" });
    const searchMessages = vi.fn(async () => [{ seq: 1, envelope: found }]);
    const source = createFakeSource({ searchMessages });

    render(<SearchBox source={source} />);

    fireEvent.change(screen.getByLabelText("전역 검색"), { target: { value: "  hello  " } });
    fireEvent.click(screen.getByRole("button", { name: "검색" }));

    await waitFor(() => expect(searchMessages).toHaveBeenCalledWith("hello"));
    await screen.findByText("hello world");
    expect(screen.getByText("task.result")).not.toBeNull();
    expect(screen.getByText("agent:pm")).not.toBeNull();
  });
});

describe("SearchBox — edge cases", () => {
  it("falls back to a client-side filter over store.messages when searchMessages is absent", async () => {
    const match = envelope({ id: "e1", kind: "task.assign", from: "agent:designer", corr: "t-designer", body: {} });
    const other = envelope({ id: "e2", kind: "task.assign", from: "agent:qa", corr: "t-qa", body: {} });
    useRunStore.setState({
      messages: [
        { seq: 1, envelope: match },
        { seq: 2, envelope: other },
      ],
    });
    const source = createFakeSource();

    render(<SearchBox source={source} />);

    fireEvent.change(screen.getByLabelText("전역 검색"), { target: { value: "designer" } });
    fireEvent.click(screen.getByRole("button", { name: "검색" }));

    await screen.findByText("agent:designer");
    expect(screen.queryByText("agent:qa")).toBeNull();
  });

  it("shows an empty-results message and closes the panel via the X button", async () => {
    const searchMessages = vi.fn(async () => []);
    const source = createFakeSource({ searchMessages });

    render(<SearchBox source={source} />);

    fireEvent.change(screen.getByLabelText("전역 검색"), { target: { value: "nothing" } });
    fireEvent.click(screen.getByRole("button", { name: "검색" }));

    await screen.findByText("결과 없음");

    fireEvent.click(screen.getByRole("button", { name: "닫기" }));

    expect(screen.queryByRole("listbox", { name: "검색 결과" })).toBeNull();
  });
});
