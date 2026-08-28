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
});
