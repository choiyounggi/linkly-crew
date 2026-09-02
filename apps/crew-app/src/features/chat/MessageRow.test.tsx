import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import MessageRow from "./MessageRow";
import type { Envelope } from "../../lib/types";

function envelope(overrides: Partial<Envelope> & Pick<Envelope, "id" | "kind" | "body">): Envelope {
  return {
    ts: "2026-08-28T00:00:00.000Z",
    sprint: "sprint-1",
    thread: "t-pm",
    from: "agent:pm",
    to: ["lead"],
    corr: "t-pm",
    artifacts: [],
    requires_ack: false,
    deadline_ms: 1000,
    ...overrides,
  };
}

const BASE_PROPS = {
  runId: "run_1",
  replyCount: 0,
  readers: [] as string[],
  gateResolution: null,
};

describe("MessageRow — kind render map (normal, D2)", () => {
  it("renders task.result: covered reqs + collapsed artifacts", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({
          id: "e1",
          kind: "task.result",
          body: { covered_req_ids: ["REQ-1"], artifacts: [{ name: "spec.md", content: "hello" }] },
        })}
      />,
    );

    expect(screen.getByText(/REQ-1/)).toBeInTheDocument();
    expect(screen.getByText("spec.md")).toBeInTheDocument();
  });

  it("renders change_request: violations + reason", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({ id: "e2", kind: "change_request", body: { violations: ["REQ-2"], reason: "dod unmet" } })}
      />,
    );

    expect(screen.getByText(/REQ-2/)).toBeInTheDocument();
    expect(screen.getByText("dod unmet")).toBeInTheDocument();
  });

  it("renders human.gate via GateCard: reason + approve/reject buttons", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({ id: "e3", kind: "human.gate", body: { task_id: "t-qa", reason: "검토 필요" } })}
      />,
    );

    expect(screen.getByText("검토 필요")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "승인" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "반려" })).toBeInTheDocument();
  });
});

describe("MessageRow — unknown/unhandled kind fallback (boundary, D2)", () => {
  it("collapses a genuinely unrecognized kind as raw JSON instead of crashing", () => {
    const weird = envelope({ id: "e4", kind: "future_kind" as never, body: { foo: "bar" } });

    expect(() => render(<MessageRow {...BASE_PROPS} envelope={weird} />)).not.toThrow();
    expect(screen.getByText("본문 보기")).toBeInTheDocument();
    expect(screen.getByText(/"foo": "bar"/)).toBeInTheDocument();
  });

  it("falls back to raw JSON when a known kind's body is malformed (error/boundary)", () => {
    const malformed = envelope({ id: "e5", kind: "task.result", body: { nope: true } });

    render(<MessageRow {...BASE_PROPS} envelope={malformed} />);

    expect(screen.getByText("본문 보기")).toBeInTheDocument();
  });
});

describe("MessageRow — presence + reply footer (normal/boundary, D1/D4)", () => {
  it("shows the read-receipt badge with a hover title when there are readers", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        readers={["agent:pm", "agent:designer"]}
        envelope={envelope({ id: "e6", kind: "task.assign", body: {} })}
      />,
    );

    const badge = screen.getByText("👀 2");
    expect(badge).toHaveAttribute("title", "agent:pm, agent:designer");
  });

  it("omits the read-receipt badge when there are no readers (boundary)", () => {
    render(<MessageRow {...BASE_PROPS} envelope={envelope({ id: "e7", kind: "task.assign", body: {} })} />);

    expect(screen.queryByText(/👀/)).not.toBeInTheDocument();
  });

  it("shows a clickable 댓글 N개 badge and calls onOpenThread with the thread id", () => {
    let opened: string | null = null;
    render(
      <MessageRow
        {...BASE_PROPS}
        replyCount={3}
        envelope={envelope({ id: "e8", kind: "task.assign", thread: "t-pm", body: {} })}
        onOpenThread={(threadId) => {
          opened = threadId;
        }}
      />,
    );

    screen.getByRole("button", { name: "댓글 3개" }).click();
    expect(opened).toBe("t-pm");
  });

  it("shows a parent-thread link for a human.gate promoted into the main stream (D1/R3 exception)", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        parentThread="t-qa"
        envelope={envelope({ id: "e9", kind: "human.gate", thread: "t-qa", body: { task_id: "t-qa", reason: "why" } })}
      />,
    );

    expect(screen.getByText("스레드: t-qa")).toBeInTheDocument();
  });
});
