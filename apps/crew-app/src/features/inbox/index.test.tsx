import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { defaultSource, useRunStore } from "../../lib/store";
import type { Envelope } from "../../lib/types";
import Inbox from "./index";

function gateMessage(seq: number, taskId: string, reason: string): { seq: number; envelope: Envelope } {
  return {
    seq,
    envelope: {
      id: `env_${seq}`,
      ts: "2026-08-30T00:00:00Z",
      sprint: "s1",
      thread: taskId,
      from: "lead",
      to: [],
      kind: "human.gate",
      corr: taskId,
      body: { task_id: taskId, reason },
      artifacts: [],
      requires_ack: false,
      deadline_ms: 60_000,
    },
  };
}

describe("Inbox", () => {
  const originalResolveGate = defaultSource.resolveGate;

  afterEach(() => {
    act(() => {
      useRunStore.setState({ messages: [], taskStates: {} });
    });
    defaultSource.resolveGate = originalResolveGate;
  });

  it("renders the empty state when there are no pending gates", () => {
    act(() => {
      useRunStore.setState({ messages: [], taskStates: {} });
    });

    render(<Inbox />);

    expect(screen.getByLabelText("승인함")).toBeInTheDocument();
    expect(screen.getByText("대기 중인 승인 요청이 없습니다")).toBeInTheDocument();
  });

  it("calls resolveGate(taskId, 'approve', reason) when 승인 is clicked", async () => {
    const resolveGate = vi.fn().mockResolvedValue(undefined);
    defaultSource.resolveGate = resolveGate;

    act(() => {
      useRunStore.setState({
        messages: [gateMessage(1, "t-a", "검토 필요")],
        taskStates: { "t-a": "escalated" },
      });
    });

    render(<Inbox />);

    fireEvent.change(screen.getByLabelText("승인/반려 사유"), { target: { value: "확인함" } });
    fireEvent.click(screen.getByText("승인"));

    await waitFor(() => expect(resolveGate).toHaveBeenCalledWith("t-a", "approve", "확인함"));
  });

  it("calls resolveGate with `this` bound to the source (regression: unbound extraction broke class sources)", async () => {
    let receivedThis: unknown = null;
    defaultSource.resolveGate = function (this: unknown) {
      receivedThis = this;
      return Promise.resolve();
    };

    act(() => {
      useRunStore.setState({
        messages: [gateMessage(1, "t-a", "검토 필요")],
        taskStates: { "t-a": "escalated" },
      });
    });

    render(<Inbox />);

    fireEvent.click(screen.getByText("승인"));

    await waitFor(() => expect(receivedThis).toBe(defaultSource));
  });

  it('shows "이 소스에서 지원 안 함" when resolveGate is not provided by the source', () => {
    defaultSource.resolveGate = undefined;

    act(() => {
      useRunStore.setState({
        messages: [gateMessage(1, "t-a", "검토 필요")],
        taskStates: { "t-a": "escalated" },
      });
    });

    render(<Inbox />);

    expect(screen.getByText("이 소스에서 지원 안 함")).toBeInTheDocument();
    expect(screen.queryByText("승인")).not.toBeInTheDocument();
  });
});
