import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { useRunStore } from "../../lib/store";
import type { Envelope, MessageKind, SpecDoc, TaskDag } from "../../lib/types";
import Artifacts from "./index";

function env(seq: number, fields: { kind: MessageKind; corr: string; body?: unknown }): { seq: number; envelope: Envelope } {
  return {
    seq,
    envelope: {
      id: `env_${seq}`,
      ts: `t${seq}`,
      sprint: "sprint-1",
      thread: fields.corr,
      from: "agent:developer",
      to: ["lead"],
      kind: fields.kind,
      corr: fields.corr,
      body: fields.body ?? {},
      artifacts: [],
      requires_ack: false,
      deadline_ms: 1000,
    },
  };
}

const SPEC: SpecDoc = {
  goal: "g",
  non_goals: [],
  constraints: [],
  requirements: [{ id: "REQ-1", text: "r1" }],
  acceptance: [],
};

const DAG: TaskDag = {
  tasks: [
    {
      id: "t-a",
      role: "developer",
      title: "A",
      brief: "b",
      dod: [],
      deps: [],
      artifacts_expected: [{ name: "spec.md", kind: "doc", req_ids: ["REQ-1"] }],
    },
  ],
};

describe("Artifacts (smoke)", () => {
  afterEach(() => {
    act(() => {
      useRunStore.setState({ spec: null, dag: null, messages: [] });
    });
  });

  it("renders section.artifacts-view with an empty state before any run data exists", () => {
    act(() => {
      useRunStore.setState({ spec: null, dag: null, messages: [] });
    });
    render(<Artifacts />);
    const view = screen.getByLabelText("아티팩트");
    expect(view.tagName).toBe("SECTION");
    expect(view).toHaveClass("artifacts-view");
    expect(screen.getByText("아티팩트 준비 중")).toBeInTheDocument();
  });

  it("renders the REQ matrix and lets selecting an artifact show its diff against the previous version", () => {
    const messages = [
      env(1, {
        kind: "task.result",
        corr: "t-a",
        body: { covered_req_ids: [], artifacts: [{ name: "spec.md", content: "a\nb" }] },
      }),
      env(2, {
        kind: "task.result",
        corr: "t-a",
        body: { covered_req_ids: ["REQ-1"], artifacts: [{ name: "spec.md", content: "a\nc" }] },
      }),
    ];
    act(() => {
      useRunStore.setState({ spec: SPEC, dag: DAG, messages });
    });
    render(<Artifacts />);

    expect(screen.getByText("REQ-1")).toBeInTheDocument();
    expect(screen.getByText("spec.md")).toBeInTheDocument();

    fireEvent.click(screen.getByText("spec.md"));

    expect(screen.getByText("b")).toHaveClass("diff-line--removed");
    expect(screen.getByText("c")).toHaveClass("diff-line--added");
    expect(screen.getByText("a")).toHaveClass("diff-line--same");
  });
});
