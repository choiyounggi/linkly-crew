import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import { useRunStore } from "../../lib/store";
import type { TaskDag } from "../../lib/types";
import DagView from "./index";

// D10: xyflow requires ResizeObserver, absent in jsdom. Stubbed here (not in the
// shared setup file, which is out of scope for features/dag).
beforeAll(() => {
  if (!("ResizeObserver" in globalThis)) {
    class ResizeObserverStub {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).ResizeObserver = ResizeObserverStub;
  }
});

const DAG: TaskDag = {
  tasks: [
    { id: "t-a", role: "developer", title: "A", brief: "b", dod: [], deps: [], artifacts_expected: [] },
    { id: "t-b", role: "qa", title: "B", brief: "b", dod: [], deps: ["t-a"], artifacts_expected: [] },
  ],
};

describe("DagView (smoke)", () => {
  afterEach(() => {
    act(() => {
      useRunStore.setState({ dag: null, taskStates: {} });
    });
  });

  it("renders without crashing and shows an empty state before any run starts", () => {
    act(() => {
      useRunStore.setState({ dag: null, taskStates: {} });
    });
    render(<DagView />);
    const view = screen.getByLabelText("DAG 뷰");
    expect(view.tagName).toBe("SECTION");
    expect(view).toHaveClass("dag-view");
  });

  it("renders a section.dag-view with a node label per task and a blocked-state tag", () => {
    act(() => {
      useRunStore.setState({ dag: DAG, taskStates: { "t-a": "accepted", "t-b": "blocked" } });
    });
    render(<DagView />);

    const view = screen.getByLabelText("DAG 뷰");
    expect(view.tagName).toBe("SECTION");
    expect(view).toHaveClass("dag-view");
    expect(screen.getByText("t-a")).toBeInTheDocument();
    expect(screen.getByText("t-b")).toBeInTheDocument();
    expect(screen.getByText("blocked")).toBeInTheDocument();
  });
});
