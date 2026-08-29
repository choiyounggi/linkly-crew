import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { useRunStore } from "../../lib/store";
import type { TaskDag } from "../../lib/types";
import Rail from "./index";

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

describe("Rail (smoke)", () => {
  afterEach(() => {
    act(() => {
      useRunStore.setState({ dag: null, taskStates: {}, messages: [], runId: null, finished: null, roster: [] });
    });
  });

  it("renders the lead card and one card per role appearing in dag.tasks, with distinct initials", () => {
    act(() => {
      useRunStore.setState({
        dag: DAG,
        taskStates: { "t-design": "pending" },
        messages: [],
        runId: "run_1",
        finished: null,
        roster: [],
      });
    });

    render(<Rail />);

    expect(screen.getByLabelText("에이전트 레일")).toBeInTheDocument();
    expect(screen.getByText("Lead")).toBeInTheDocument();
    expect(screen.getByText("Designer")).toBeInTheDocument();
    expect(screen.getAllByText("claude-code")).toHaveLength(2);
    expect(screen.getByText("LD")).toBeInTheDocument();
    expect(screen.getByText("DS")).toBeInTheDocument();
  });

  it("shows a roster-provided harness instead of the claude-code fallback", () => {
    act(() => {
      useRunStore.setState({
        dag: DAG,
        taskStates: { "t-design": "pending" },
        messages: [],
        runId: "run_1",
        finished: null,
        roster: [{ id: "agent:designer", role: "designer", harness: "opencode", model: "m" }],
      });
    });

    render(<Rail />);

    expect(screen.getByText("opencode")).toBeInTheDocument();
    expect(screen.getByText("claude-code")).toBeInTheDocument(); // lead still on fallback
  });

  it("renders just the lead card with no crash before any run starts", () => {
    act(() => {
      useRunStore.setState({ dag: null, taskStates: {}, messages: [], runId: null, finished: null, roster: [] });
    });
    render(<Rail />);
    expect(screen.getByLabelText("에이전트 레일")).toBeInTheDocument();
    expect(screen.getByText("Lead")).toBeInTheDocument();
  });
});
