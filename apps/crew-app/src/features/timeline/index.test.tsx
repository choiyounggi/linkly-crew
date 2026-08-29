import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import TimelineView from "./index";
import { useRunStore } from "../../lib/store";
import type { Envelope } from "../../lib/types";

function envelope(
  overrides: Partial<Envelope> & Pick<Envelope, "id" | "kind" | "from" | "ts">,
): Envelope {
  return {
    sprint: "sprint-1",
    thread: "t-pm",
    to: ["x"],
    corr: "t-pm",
    in_reply_to: undefined,
    body: {},
    artifacts: [],
    requires_ack: false,
    deadline_ms: 1000,
    ...overrides,
  };
}

beforeEach(() => {
  useRunStore.setState({ messages: [], sprintWindows: [] });
});

describe("TimelineView — normal", () => {
  it("renders a lane per agent with chips positioned by frac and a hover title", () => {
    const assign = envelope({
      id: "e1",
      kind: "task.assign",
      from: "agent:pm",
      ts: "2024-01-01T00:00:00.000Z",
      body: { task: { id: "t1" } },
    });
    const result = envelope({
      id: "e2",
      kind: "task.result",
      from: "agent:pm",
      ts: "2024-01-01T00:00:10.000Z",
    });

    useRunStore.setState({
      messages: [
        { seq: 1, envelope: assign },
        { seq: 2, envelope: result },
      ],
      sprintWindows: [{ index: 0, startTs: "2024-01-01T00:00:00.000Z", endTs: null }],
    });

    render(<TimelineView />);

    expect(screen.getByText("pm")).toBeInTheDocument();
    expect(screen.getByText("S0")).toBeInTheDocument();
    expect(document.querySelectorAll(".timeline-chip")).toHaveLength(2);
    expect(document.querySelector('[title="2024-01-01T00:00:00.000Z task.assign t1"]')).not.toBeNull();
  });
});

describe("TimelineView — boundary", () => {
  it("shows a placeholder and does not crash with no messages", () => {
    render(<TimelineView />);
    expect(screen.getByText("아직 활동이 없습니다")).toBeInTheDocument();
    expect(document.querySelectorAll(".timeline-chip")).toHaveLength(0);
  });

  it("keeps a section.timeline-view root in the empty state (contracts-m6 D5 amendment)", () => {
    const { container } = render(<TimelineView />);
    expect(container.querySelector("section.timeline-view")).not.toBeNull();
  });

  it("keeps a section.timeline-view root in the populated state (contracts-m6 D5 amendment)", () => {
    useRunStore.setState({
      messages: [
        {
          seq: 1,
          envelope: envelope({ id: "e1", kind: "task.assign", from: "agent:pm", ts: "2024-01-01T00:00:00.000Z" }),
        },
      ],
      sprintWindows: [],
    });
    const { container } = render(<TimelineView />);
    expect(container.querySelector("section.timeline-view")).not.toBeNull();
  });
});
