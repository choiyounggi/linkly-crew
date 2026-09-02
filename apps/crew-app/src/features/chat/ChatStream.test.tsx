import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import ChatStream from "./ChatStream";
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

describe("ChatStream — root-only render with reply badge (normal, D1/D8)", () => {
  it("renders only the thread root, folding replies into a 댓글 N개 badge", () => {
    setChannel({
      messages: [
        { seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-pm", body: {} }) },
        { seq: 2, envelope: envelope({ id: "e2", kind: "task.ack", thread: "t-pm", body: {} }) },
      ],
    });

    render(<ChatStream runId={RUN_ID} />);

    expect(screen.getAllByRole("listitem")).toHaveLength(1);
    expect(screen.getByText("댓글 1개")).toBeInTheDocument();
  });
});

describe("ChatStream — empty channel (boundary)", () => {
  it("shows the empty-state message when there are no messages yet", () => {
    render(<ChatStream runId={RUN_ID} />);

    expect(screen.getByText("아직 메시지가 없습니다")).toBeInTheDocument();
  });

  it("shows the empty-state message for a runId with no channel at all (boundary)", () => {
    render(<ChatStream runId="run_ghost" />);

    expect(screen.getByText("아직 메시지가 없습니다")).toBeInTheDocument();
  });
});

describe("ChatStream — human.gate main-stream exception (normal, D1/R3)", () => {
  it("shows a human.gate directly in the main stream even though it is a reply on its thread", () => {
    setChannel({
      messages: [
        { seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-qa", body: {} }) },
        {
          seq: 2,
          envelope: envelope({
            id: "e2",
            kind: "human.gate",
            thread: "t-qa",
            body: { task_id: "t-qa", reason: "qa 차단 사유 검토 필요" },
          }),
        },
      ],
    });

    render(<ChatStream runId={RUN_ID} />);

    expect(screen.getAllByRole("listitem")).toHaveLength(2);
    expect(screen.getByText("qa 차단 사유 검토 필요")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "승인" })).toBeInTheDocument();
  });
});

describe("ChatStream — read receipts (normal, D4)", () => {
  it("shows the 👀 badge for a root message that has readers", () => {
    setChannel({
      messages: [{ seq: 1, envelope: envelope({ id: "e1", kind: "task.assign", thread: "t-pm", body: {} }) }],
      readReceipts: { e1: ["agent:pm"] },
    });

    render(<ChatStream runId={RUN_ID} />);

    expect(screen.getByText("👀 1")).toBeInTheDocument();
  });
});
