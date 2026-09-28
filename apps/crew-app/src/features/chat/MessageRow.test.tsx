import { fireEvent, render, screen } from "@testing-library/react";
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

    expect(screen.getByText("커버: REQ-1")).toBeInTheDocument();
    expect(screen.getByText("spec.md")).toBeInTheDocument();
  });

  it("renders change_request: violations + reason", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({ id: "e2", kind: "change_request", body: { violations: ["REQ-2"], reason: "dod unmet" } })}
      />,
    );

    expect(screen.getByText("위반: REQ-2")).toBeInTheDocument();
    expect(screen.getByText("dod unmet")).toBeInTheDocument();
  });

  it("renders human.gate as a sentence (REPLACES the old GateCard render — GateCard is gone from this row)", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({ id: "e3", kind: "human.gate", body: { task_id: "t-qa", reason: "검토 필요" } })}
      />,
    );

    expect(screen.getByText("@human님, t-qa 검토가 필요합니다: 검토 필요")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "승인" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "반려" })).not.toBeInTheDocument();
  });

  it("renders task.assign: mentioned recipient + title", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({ id: "e10", kind: "task.assign", to: ["developer"], body: { task: { title: "로그인 페이지 구현" } } })}
      />,
    );

    expect(screen.getByText("@developer님, 로그인 페이지 구현 진행해 주세요")).toBeInTheDocument();
  });

  it("renders task.assign fallback when the body is malformed", () => {
    render(<MessageRow {...BASE_PROPS} envelope={envelope({ id: "e11", kind: "task.assign", to: [], body: {} })} />);

    expect(screen.getByText("작업을 진행해 주세요")).toBeInTheDocument();
  });

  it("renders task.assign with a title but no recipient (to empty)", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({ id: "e11a", kind: "task.assign", to: [], body: { task: { title: "로그인 페이지 구현" } } })}
      />,
    );

    expect(screen.getByText("로그인 페이지 구현 진행해 주세요")).toBeInTheDocument();
  });

  it("renders task.assign with a recipient but a missing title", () => {
    render(
      <MessageRow {...BASE_PROPS} envelope={envelope({ id: "e11b", kind: "task.assign", to: ["developer"], body: {} })} />,
    );

    expect(screen.getByText("@developer님, 작업을 진행해 주세요")).toBeInTheDocument();
  });

  it("renders task.progress: summary line", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({ id: "e12", kind: "task.progress", body: { summary: "구현 60% 진행" } })}
      />,
    );

    expect(screen.getByText("구현 60% 진행")).toBeInTheDocument();
  });

  it("renders task.progress fallback when the body is malformed", () => {
    render(<MessageRow {...BASE_PROPS} envelope={envelope({ id: "e12a", kind: "task.progress", body: {} })} />);

    expect(screen.getByText("진행 상황이 업데이트되었습니다")).toBeInTheDocument();
  });

  it("renders review.request: mentioned recipient", () => {
    render(
      <MessageRow {...BASE_PROPS} envelope={envelope({ id: "e13", kind: "review.request", to: ["qa"], body: {} })} />,
    );

    expect(screen.getByText("@qa님 리뷰 요청")).toBeInTheDocument();
  });

  it("renders review.request fallback when to is empty", () => {
    render(<MessageRow {...BASE_PROPS} envelope={envelope({ id: "e13a", kind: "review.request", to: [], body: {} })} />);

    expect(screen.getByText("리뷰 요청")).toBeInTheDocument();
  });

  it("renders question: prefixed text", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({ id: "e14", kind: "question", body: { text: "반응형 기준이 뭔가요?" } })}
      />,
    );

    expect(screen.getByText("❓ 반응형 기준이 뭔가요?")).toBeInTheDocument();
  });

  it("renders question fallback when the body is malformed", () => {
    render(<MessageRow {...BASE_PROPS} envelope={envelope({ id: "e14a", kind: "question", body: {} })} />);

    expect(screen.getByText("❓ (내용 없음)")).toBeInTheDocument();
  });

  it("renders answer: prefixed text", () => {
    render(
      <MessageRow {...BASE_PROPS} envelope={envelope({ id: "e15", kind: "answer", body: { text: "375px 기준 1열입니다" } })} />,
    );

    expect(screen.getByText("💬 375px 기준 1열입니다")).toBeInTheDocument();
  });

  it("renders answer fallback when the body is malformed", () => {
    render(<MessageRow {...BASE_PROPS} envelope={envelope({ id: "e15a", kind: "answer", body: {} })} />);

    expect(screen.getByText("💬 (내용 없음)")).toBeInTheDocument();
  });

  it("renders blocked: reason", () => {
    render(
      <MessageRow {...BASE_PROPS} envelope={envelope({ id: "e16", kind: "blocked", body: { reason: "의존성 대기 중" } })} />,
    );

    expect(screen.getByText("🚧 차단: 의존성 대기 중")).toBeInTheDocument();
  });

  it("renders blocked fallback when the body is malformed", () => {
    render(<MessageRow {...BASE_PROPS} envelope={envelope({ id: "e16a", kind: "blocked", body: {} })} />);

    expect(screen.getByText("🚧 차단됨")).toBeInTheDocument();
  });

  it("renders handoff: role", () => {
    render(
      <MessageRow {...BASE_PROPS} envelope={envelope({ id: "e17", kind: "handoff", body: { pack: { role: "developer" } } })} />,
    );

    expect(screen.getByText("🔄 인수인계: developer")).toBeInTheDocument();
  });

  it("renders handoff fallback when the body is malformed", () => {
    render(<MessageRow {...BASE_PROPS} envelope={envelope({ id: "e17a", kind: "handoff", body: {} })} />);

    expect(screen.getByText("🔄 인수인계됨")).toBeInTheDocument();
  });

  it("renders human.response: approve decision + reason", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({
          id: "e18",
          kind: "human.response",
          body: { task_id: "t-qa", decision: "approve", reason: "ok" },
        })}
      />,
    );

    expect(screen.getByText("승인됨 — ok")).toBeInTheDocument();
  });

  it("renders human.response: reject decision + reason", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({
          id: "e18a",
          kind: "human.response",
          body: { task_id: "t-qa", decision: "reject", reason: "no" },
        })}
      />,
    );

    expect(screen.getByText("반려됨 — no")).toBeInTheDocument();
  });

  it("renders human.response: approve with an empty reason omits the em dash", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({
          id: "e18b",
          kind: "human.response",
          body: { task_id: "t-qa", decision: "approve", reason: "" },
        })}
      />,
    );

    expect(screen.getByText("승인됨")).toBeInTheDocument();
    expect(screen.queryByText(/승인됨 —/)).not.toBeInTheDocument();
  });

  it("renders human.response fallback when the body is malformed", () => {
    render(<MessageRow {...BASE_PROPS} envelope={envelope({ id: "e18c", kind: "human.response", body: {} })} />);

    expect(screen.getByText("응답이 도착했습니다")).toBeInTheDocument();
  });

  it("renders human.gate fallback when the body is malformed", () => {
    render(<MessageRow {...BASE_PROPS} envelope={envelope({ id: "e3a", kind: "human.gate", body: {} })} />);

    expect(screen.getByText("@human님, 검토가 필요합니다")).toBeInTheDocument();
  });
});

describe("MessageRow — task.ack renders no row at all (boundary, D2)", () => {
  it("returns null for task.ack — no <li> is rendered", () => {
    const { container } = render(
      <MessageRow {...BASE_PROPS} envelope={envelope({ id: "e19", kind: "task.ack", body: {} })} />,
    );

    expect(container.querySelector("li")).not.toBeInTheDocument();
    expect(container).toBeEmptyDOMElement();
  });
});

describe("MessageRow — unknown/unhandled kind fallback (boundary, D2)", () => {
  it("collapses a genuinely unrecognized kind as raw JSON instead of crashing (본문 보기 renamed to 원문)", () => {
    const weird = envelope({ id: "e4", kind: "future_kind" as never, body: { foo: "bar" } });

    expect(() => render(<MessageRow {...BASE_PROPS} envelope={weird} />)).not.toThrow();
    expect(screen.getByText("알 수 없는 메시지 종류: future_kind")).toBeInTheDocument();
    expect(screen.getByText("원문")).toBeInTheDocument();
    expect(screen.getByText(/"foo": "bar"/)).toBeInTheDocument();
  });

  it("renders task.result's OWN malformed-body fallback, not the unknown-kind label (F1 fix — REPLACES the weaker toggle-only assertion)", () => {
    const malformed = envelope({ id: "e5", kind: "task.result", body: { nope: true } });

    render(<MessageRow {...BASE_PROPS} envelope={malformed} />);

    expect(screen.getByText("작업 결과가 도착했습니다 (본문 형식 오류)")).toBeInTheDocument();
    expect(screen.queryByText(/알 수 없는 메시지 종류/)).not.toBeInTheDocument();
    expect(screen.getByText("원문")).toBeInTheDocument();
  });

  it("renders change_request's OWN malformed-body fallback, not the unknown-kind label (F1 fix)", () => {
    const malformed = envelope({ id: "e5b", kind: "change_request", body: { nope: true } });

    render(<MessageRow {...BASE_PROPS} envelope={malformed} />);

    expect(screen.getByText("변경 요청 (본문 형식 오류)")).toBeInTheDocument();
    expect(screen.queryByText(/알 수 없는 메시지 종류/)).not.toBeInTheDocument();
    expect(screen.getByText("원문")).toBeInTheDocument();
  });
});

describe("MessageRow — 원문 raw-JSON toggle mechanics (normal/boundary, D5)", () => {
  it("stays collapsed by default and reveals the raw JSON only after clicking 원문, for task.assign", () => {
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({ id: "e20", kind: "task.assign", to: ["developer"], body: { task: { title: "구현" } } })}
      />,
    );

    const raw = screen.getByText(/"task"/);
    expect(raw).not.toBeVisible();

    fireEvent.click(screen.getByText("원문"));

    expect(raw).toBeVisible();
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

describe("MessageRow — @mention chips from envelope.to (normal/boundary, D4)", () => {
  it("renders one chip per recipient and opens the row's own thread on click", () => {
    let opened: string | null = null;
    render(
      <MessageRow
        {...BASE_PROPS}
        envelope={envelope({ id: "e21", kind: "task.assign", thread: "t-pm", to: ["pm", "developer"], body: {} })}
        onOpenThread={(threadId) => {
          opened = threadId;
        }}
      />,
    );

    expect(screen.getByRole("button", { name: "@pm" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "@developer" })).toBeInTheDocument();

    screen.getByRole("button", { name: "@developer" }).click();
    expect(opened).toBe("t-pm");
  });

  it("renders no mention-chip container when to is empty (boundary)", () => {
    const { container } = render(
      <MessageRow {...BASE_PROPS} envelope={envelope({ id: "e22", kind: "task.assign", to: [], body: {} })} />,
    );

    expect(container.querySelector(".message-row__mentions")).not.toBeInTheDocument();
  });

  it("renders a non-interactive span chip (not a button) when onOpenThread is not provided", () => {
    const { container } = render(
      <MessageRow {...BASE_PROPS} envelope={envelope({ id: "e23", kind: "task.assign", to: ["pm"], body: {} })} />,
    );

    expect(container.querySelector("button.message-row__mention-chip")).not.toBeInTheDocument();
    const chip = container.querySelector("span.message-row__mention-chip");
    expect(chip).toBeInTheDocument();
    expect(chip).toHaveTextContent("@pm");
  });
});
