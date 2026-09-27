import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import ThreadPanel from "./ThreadPanel";
import { emptyChannel, useRunStore } from "../../lib/store";
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

describe("ThreadPanel — portal mount (normal, App.tsx aside contract)", () => {
  let target: HTMLElement;

  beforeEach(() => {
    target = document.createElement("aside");
    target.id = "channel-side-panel";
    document.body.appendChild(target);
  });

  afterEach(() => {
    target.remove();
  });

  it("renders into the #channel-side-panel target via portal", () => {
    setChannel({
      messages: [{ seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-pm", body: {} }) }],
    });

    render(<ThreadPanel runId={RUN_ID} threadId="t-pm" onClose={() => {}} />);

    expect(target.querySelector(".thread-panel")).not.toBeNull();
    expect(target.textContent).toContain("스레드: t-pm");
  });

  it("calls onClose when the close button is clicked", () => {
    let closed = false;
    render(<ThreadPanel runId={RUN_ID} threadId="t-pm" onClose={() => (closed = true)} />);

    fireEvent.click(screen.getByRole("button", { name: "스레드 패널 닫기" }));

    expect(closed).toBe(true);
  });
});

describe("ThreadPanel — inline fallback when no portal target is mounted (boundary)", () => {
  it("still renders (does not throw or disappear) when #channel-side-panel is absent", () => {
    setChannel({
      messages: [{ seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-pm", body: {} }) }],
    });

    expect(document.getElementById("channel-side-panel")).toBeNull();
    const { container } = render(<ThreadPanel runId={RUN_ID} threadId="t-pm" onClose={() => {}} />);

    expect(container.querySelector(".thread-panel")).not.toBeNull();
    expect(screen.getByText("스레드: t-pm")).toBeInTheDocument();
  });
});

describe("ThreadPanel — timeline filtering (normal/boundary)", () => {
  it("shows only messages belonging to the selected thread", () => {
    setChannel({
      messages: [
        { seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-pm", body: {} }) },
        { seq: 2, envelope: envelope({ id: "e2", kind: "task.assign", thread: "t-other", body: {} }) },
      ],
    });

    render(<ThreadPanel runId={RUN_ID} threadId="t-pm" onClose={() => {}} />);

    expect(screen.getAllByRole("listitem")).toHaveLength(1);
  });

  it("shows an empty-state message for a thread with no messages (boundary)", () => {
    render(<ThreadPanel runId={RUN_ID} threadId="t-ghost" onClose={() => {}} />);

    expect(screen.getByText("메시지가 없습니다")).toBeInTheDocument();
    expect(document.querySelector(".thread-panel__list")).toBeNull();
  });
});

describe("ThreadPanel — list container structure (issue #29 regression pin)", () => {
  it("renders exactly one .thread-panel__list containing exactly one row (normal)", () => {
    setChannel({
      messages: [{ seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-single", body: {} }) }],
    });

    const { container } = render(<ThreadPanel runId={RUN_ID} threadId="t-single" onClose={() => {}} />);

    expect(container.querySelectorAll(".thread-panel__list")).toHaveLength(1);
    expect(container.querySelectorAll(".thread-panel__list > li.message-row")).toHaveLength(1);
  });

  it("renders exactly one .thread-panel__list containing 500 rows, no second scroll wrapper (boundary)", () => {
    const messages = Array.from({ length: 500 }, (_, i) => ({
      seq: i + 1,
      envelope: envelope({ id: `e${i}`, kind: "task.assign", thread: "t-many", body: {} }),
    }));
    setChannel({ messages });

    const { container } = render(<ThreadPanel runId={RUN_ID} threadId="t-many" onClose={() => {}} />);

    expect(container.querySelectorAll(".thread-panel__list")).toHaveLength(1);
    expect(container.querySelectorAll(".thread-panel__list > li.message-row")).toHaveLength(500);
  });

  it("still renders exactly one .thread-panel__list when a human.gate message has an empty body (error)", () => {
    setChannel({
      messages: [
        { seq: 1, envelope: envelope({ id: "e-gate", kind: "human.gate", thread: "t-gate", body: {} }) },
      ],
    });

    expect(() =>
      render(<ThreadPanel runId={RUN_ID} threadId="t-gate" onClose={() => {}} />),
    ).not.toThrow();
    expect(document.querySelectorAll(".thread-panel__list")).toHaveLength(1);
  });
});
