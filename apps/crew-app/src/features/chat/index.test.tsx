import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import ChatPane from "./index";
import { defaultSource, emptyChannel, useRunStore } from "../../lib/store";
import type { Envelope } from "../../lib/types";

const RUN_ID = "run_test";

function setChannel(overrides: Partial<ReturnType<typeof emptyChannel>>) {
  useRunStore.setState({
    activeRunId: RUN_ID,
    channelOrder: [RUN_ID],
    channels: { [RUN_ID]: { ...emptyChannel(RUN_ID, "goal", false), ...overrides } },
  });
}

function envelope(overrides: Partial<Envelope> & Pick<Envelope, "id" | "kind" | "thread" | "body">): Envelope {
  return {
    ts: "2026-08-28T00:00:00.000Z",
    sprint: "sprint-1",
    from: "lead",
    to: ["agent:pm"],
    corr: overrides.thread,
    artifacts: [],
    requires_ack: false,
    deadline_ms: 1000,
    ...overrides,
  };
}

beforeEach(() => {
  setChannel({});
});

describe("ChatPane — typing indicator (normal/boundary, D5)", () => {
  it("shows the agents currently typing", () => {
    setChannel({ typing: { "agent:pm": true, "agent:designer": false } });

    render(<ChatPane runId={RUN_ID} />);

    expect(screen.getByText("agent:pm 입력 중...")).toBeInTheDocument();
  });

  it("renders nothing when no one is typing (boundary)", () => {
    setChannel({ typing: {} });

    render(<ChatPane runId={RUN_ID} />);

    expect(screen.queryByText(/입력 중/)).not.toBeInTheDocument();
  });
});

describe("ChatPane — composer reflects the store's active gate (normal/boundary, D6)", () => {
  it("enables the composer when the channel has an unresolved human.gate", () => {
    setChannel({
      messages: [
        {
          seq: 1,
          envelope: envelope({ id: "e1", kind: "human.gate", thread: "t-qa", body: { task_id: "t-qa", reason: "why" } }),
        },
      ],
    });

    render(<ChatPane runId={RUN_ID} />);

    expect(screen.getByLabelText("게이트 응답")).not.toBeDisabled();
  });

  it("disables the composer when there is no active gate (boundary, D6)", () => {
    render(<ChatPane runId={RUN_ID} />);

    expect(screen.getByPlaceholderText("@멘션 2차 예정")).toBeDisabled();
  });
});

describe("ChatPane — thread panel and search wiring (normal, D1/D9, Task 03)", () => {
  it("opens the thread panel when a stream row's 댓글 N개 badge is clicked", () => {
    setChannel({
      messages: [
        { seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-pm", body: {} }) },
        { seq: 2, envelope: envelope({ id: "e2", kind: "task.ack", thread: "t-pm", body: {} }) },
      ],
    });

    render(<ChatPane runId={RUN_ID} />);
    expect(screen.queryByLabelText("스레드 패널")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "댓글 1개" }));

    expect(screen.getByLabelText("스레드 패널")).toBeInTheDocument();
    expect(screen.getByText("스레드: t-pm")).toBeInTheDocument();
  });

  it("opens the search overlay from the toolbar's 검색 button", () => {
    render(<ChatPane runId={RUN_ID} />);
    expect(screen.queryByLabelText("채널 검색")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "검색" }));

    expect(screen.getByLabelText("채널 검색")).toBeInTheDocument();
  });
});

describe("ChatPane — clicking a search result both opens its thread and closes the overlay (normal, D1 x D9 integration)", () => {
  const originalSearchMessages = defaultSource.searchMessages;

  afterEach(() => {
    defaultSource.searchMessages = originalSearchMessages;
  });

  it("opens ThreadPanel for the result's thread id and unmounts SearchOverlay in the same click", async () => {
    setChannel({
      messages: [{ seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-pm", body: {} }) }],
    });
    // ChatPaneProps is a stable {runId}-only surface (t6 contract) — SearchOverlay
    // can't be handed an injected source through ChatPane, so this stubs the
    // real defaultSource it falls back to, same as every other chat/ component.
    defaultSource.searchMessages = vi.fn().mockResolvedValue([
      {
        seq: 5,
        envelope: envelope({ id: "e5", kind: "task.result", thread: "t-pm", body: {}, from: "agent:qa" }),
      },
    ]);

    render(<ChatPane runId={RUN_ID} />);
    fireEvent.click(screen.getByRole("button", { name: "검색" }));
    fireEvent.change(screen.getByLabelText("메시지 검색"), { target: { value: "pm" } });

    const result = await screen.findByRole("button", { name: /agent:qa/ });
    fireEvent.click(result);

    expect(screen.getByLabelText("스레드 패널")).toBeInTheDocument();
    expect(screen.getByText("스레드: t-pm")).toBeInTheDocument();
    expect(screen.queryByLabelText("채널 검색")).not.toBeInTheDocument();
  });
});
