import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { useRunStore } from "../../lib/store";
import type { TaskDag } from "../../lib/types";
import Board from "./index";

const DAG: TaskDag = {
  tasks: [
    {
      id: "t-design",
      role: "designer",
      title: "디자인 시안 작업",
      brief: "brief",
      dod: [],
      deps: [],
      artifacts_expected: [],
    },
  ],
};

const BLOCKED_DAG: TaskDag = {
  tasks: [
    {
      id: "t-escalated",
      role: "developer",
      title: "에스컬레이션된 작업",
      brief: "brief",
      dod: [],
      deps: [],
      artifacts_expected: [],
    },
    {
      id: "t-blocked",
      role: "qa",
      title: "차단된 작업",
      brief: "brief",
      dod: [],
      deps: [],
      artifacts_expected: [],
    },
  ],
};

describe("Board (smoke)", () => {
  afterEach(() => {
    act(() => {
      useRunStore.setState({ dag: null, taskStates: {}, messages: [] });
    });
  });

  it("renders all 5 columns and a card for each dag task in the right column", () => {
    act(() => {
      useRunStore.setState({ dag: DAG, taskStates: { "t-design": "pending" }, messages: [] });
    });

    render(<Board />);

    expect(screen.getByLabelText("스프린트 보드")).toBeInTheDocument();
    for (const label of ["대기", "진행", "검토", "완료", "차단"]) {
      expect(screen.getByLabelText(label)).toBeInTheDocument();
    }
    expect(screen.getByText("디자인 시안 작업")).toBeInTheDocument();
  });

  it("renders an empty board with no crash before any run starts", () => {
    act(() => {
      useRunStore.setState({ dag: null, taskStates: {}, messages: [] });
    });
    render(<Board />);
    expect(screen.getByLabelText("스프린트 보드")).toBeInTheDocument();
  });

  it("shows the avatar initial (not a colliding single letter) for a designer card", () => {
    act(() => {
      useRunStore.setState({ dag: DAG, taskStates: { "t-design": "pending" }, messages: [] });
    });
    render(<Board />);
    expect(screen.getByTitle("designer")).toHaveTextContent("DS");
  });

  it("shows escalated and blocked cards in 차단, each labeled with its own text", () => {
    act(() => {
      useRunStore.setState({
        dag: BLOCKED_DAG,
        taskStates: { "t-escalated": "escalated", "t-blocked": "blocked" },
        messages: [],
      });
    });
    render(<Board />);

    const blocked = screen.getByLabelText("차단");
    expect(blocked).toHaveTextContent("에스컬레이션된 작업");
    expect(blocked).toHaveTextContent("차단된 작업");
    expect(screen.getByText("escalated")).toBeInTheDocument();
    expect(screen.getByText("blocked")).toBeInTheDocument();
  });
});
