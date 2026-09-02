import { render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import Thread from "./index";
import { emptyChannel, useRunStore } from "../../lib/store";
import type { Envelope } from "../../lib/types";

// Mechanical adaptation to the multi-run store (plan D1): sets the active
// channel's messages instead of the old flat field. Same envelopes/assertions.
const RUN_ID = "run_test";

function setMessages(messages: { seq: number; envelope: Envelope }[]) {
  useRunStore.setState({
    activeRunId: RUN_ID,
    channels: { [RUN_ID]: { ...emptyChannel(RUN_ID, "goal", false), messages } },
  });
}

function envelope(
  overrides: Partial<Envelope> & Pick<Envelope, "id" | "kind" | "from" | "to" | "corr">,
): Envelope {
  return {
    ts: "2026-08-28T00:00:00.000Z",
    sprint: "sprint-1",
    thread: "t-pm",
    in_reply_to: undefined,
    body: {},
    artifacts: [],
    requires_ack: false,
    deadline_ms: 1000,
    ...overrides,
  };
}

beforeEach(() => {
  setMessages([]);
});

describe("Thread — normal rendering", () => {
  it("renders messages in seq order, badges the kind, and folds the ack into the assign row", () => {
    const assign = envelope({
      id: "e1",
      kind: "task.assign",
      from: "lead",
      to: ["agent:pm"],
      corr: "t-pm",
      body: { task: {} },
    });
    const ack = envelope({
      id: "e2",
      kind: "task.ack",
      from: "agent:pm",
      to: ["lead"],
      corr: "t-pm",
      in_reply_to: "e1",
      body: {},
    });
    const result = envelope({
      id: "e3",
      kind: "task.result",
      from: "agent:pm",
      to: ["lead"],
      corr: "t-pm",
      in_reply_to: "e2",
      body: { covered_req_ids: ["REQ-1"], artifacts: [] },
    });

    setMessages([
      { seq: 1, envelope: assign },
      { seq: 2, envelope: ack },
      { seq: 3, envelope: result },
    ]);

    render(<Thread />);

    const rows = screen.getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(within(rows[0]).getByText("task.assign")).toBeInTheDocument();
    expect(within(rows[0]).getByText("👀 agent:pm")).toBeInTheDocument();
    expect(within(rows[1]).getByText("task.result")).toBeInTheDocument();
  });

  it("highlights change_request rows", () => {
    const cr = envelope({
      id: "e1",
      kind: "change_request",
      from: "lead",
      to: ["agent:designer"],
      corr: "t-design",
      body: { violations: ["REQ-2"], reason: "dod unmet" },
    });
    setMessages([{ seq: 1, envelope: cr }]);

    render(<Thread />);
    expect(screen.getByRole("listitem")).toHaveClass("thread-row--danger");
  });

  it("applies a warning style to human.gate rows", () => {
    const gate = envelope({
      id: "e1",
      kind: "human.gate",
      from: "lead",
      to: ["human"],
      corr: "t-pm",
      body: {},
    });
    setMessages([{ seq: 1, envelope: gate }]);

    render(<Thread />);
    expect(screen.getByRole("listitem")).toHaveClass("thread-row--warning");
  });
});

describe("Thread — boundary", () => {
  it("shows a placeholder and does not crash with no messages", () => {
    render(<Thread />);
    expect(screen.getByText("아직 메시지가 없습니다")).toBeInTheDocument();
    expect(screen.queryByRole("listitem")).not.toBeInTheDocument();
  });

  it("falls back to raw JSON for a malformed task.result body without crashing", () => {
    const bad = envelope({
      id: "e1",
      kind: "task.result",
      from: "agent:pm",
      to: ["lead"],
      corr: "t-pm",
      body: { unexpected: true },
    });
    setMessages([{ seq: 1, envelope: bad }]);

    expect(() => render(<Thread />)).not.toThrow();
    expect(screen.getByText('{"unexpected":true}')).toBeInTheDocument();
  });

  it("renders an empty artifacts list without crashing", () => {
    const result = envelope({
      id: "e1",
      kind: "task.result",
      from: "agent:pm",
      to: ["lead"],
      corr: "t-pm",
      body: { covered_req_ids: [], artifacts: [] },
    });
    setMessages([{ seq: 1, envelope: result }]);

    expect(() => render(<Thread />)).not.toThrow();
  });
});

describe("Thread — ack compression", () => {
  it("never renders task.ack as its own row", () => {
    const assign = envelope({
      id: "e1",
      kind: "task.assign",
      from: "lead",
      to: ["agent:pm"],
      corr: "t-pm",
    });
    const ack = envelope({
      id: "e2",
      kind: "task.ack",
      from: "agent:pm",
      to: ["lead"],
      corr: "t-pm",
      in_reply_to: "e1",
    });
    setMessages([
      { seq: 1, envelope: assign },
      { seq: 2, envelope: ack },
    ]);

    render(<Thread />);

    expect(screen.queryByText("task.ack")).not.toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
  });
});
