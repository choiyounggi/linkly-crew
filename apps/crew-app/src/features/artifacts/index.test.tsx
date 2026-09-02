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

  // 세 상태의 CSS 훅은 artifacts.css 가 각각 다르게 칠하는 유일한 접점이다
  // (--none 은 채움 대신 테두리로 구분한다). 클래스가 조용히 사라지면 화면에서
  // 상태 구분이 사라지므로, 렌더된 클래스 자체를 계약으로 못 박는다.
  it("셀마다 covered/expected/none 세 상태 클래스를 각각 렌더한다", () => {
    const spec: SpecDoc = { ...SPEC, requirements: [{ id: "REQ-1", text: "r1" }, { id: "REQ-2", text: "r2" }] };
    const dag: TaskDag = {
      tasks: [
        { ...DAG.tasks[0], artifacts_expected: [{ name: "spec.md", kind: "doc", req_ids: ["REQ-1", "REQ-2"] }] },
        { id: "t-b", role: "qa", title: "B", brief: "b", dod: [], deps: [],
          artifacts_expected: [{ name: "qa.md", kind: "report", req_ids: [] }] },
      ],
    };
    const messages = [
      env(1, { kind: "task.result", corr: "t-a", body: { covered_req_ids: ["REQ-1"], artifacts: [] } }),
    ];
    act(() => {
      useRunStore.setState({ spec, dag, messages });
    });
    render(<Artifacts />);

    const cell = (label: string) => screen.getByLabelText(label);
    expect(cell("REQ-1 t-a covered")).toHaveClass("req-matrix__cell--covered");
    expect(cell("REQ-2 t-a expected")).toHaveClass("req-matrix__cell--expected");
    expect(cell("REQ-1 t-b none")).toHaveClass("req-matrix__cell--none");
    expect(cell("REQ-2 t-b none")).toHaveClass("req-matrix__cell--none");

    // 세 클래스가 서로 다른 셀에 붙어야 한다 — 하나로 합쳐지면 위 네 단언 중
    // 셋이 같은 노드를 가리키게 되므로, 노드가 실제로 4개인지도 함께 확인한다
    expect(document.querySelectorAll(".req-matrix__cell").length).toBe(4);
    expect(new Set([cell("REQ-1 t-a covered"), cell("REQ-2 t-a expected"), cell("REQ-1 t-b none")]).size).toBe(3);
  });
});
