import { describe, expect, it } from "vitest";

import type { TaskDag, TaskSpec } from "../../lib/types";
import { avatarInitials, buildDagView } from "./derive";

function task(id: string, deps: string[]): TaskSpec {
  return { id, role: "developer", title: id, brief: "brief", dod: [], deps, artifacts_expected: [] };
}

describe("buildDagView", () => {
  it("computes layer/row/criticalPath for a linear chain A→B→C", () => {
    const dag: TaskDag = { tasks: [task("a", []), task("b", ["a"]), task("c", ["b"])] };

    const { nodes, edges, criticalPath } = buildDagView(dag, {});

    const byId = Object.fromEntries(nodes.map((n) => [n.id, n]));
    expect(byId.a.layer).toBe(0);
    expect(byId.b.layer).toBe(1);
    expect(byId.c.layer).toBe(2);
    expect(byId.a.row).toBe(0);
    expect(byId.b.row).toBe(0);
    expect(byId.c.row).toBe(0);
    expect(criticalPath).toEqual(["a", "b", "c"]);
    expect(edges).toHaveLength(2);
    expect(edges.every((e) => e.onCriticalPath)).toBe(true);
    expect(nodes.every((n) => n.onCriticalPath)).toBe(true);
  });

  it("breaks a tied-length diamond A→{B,C}→D lexicographically (b<c ⇒ [a,b,d])", () => {
    const dag: TaskDag = { tasks: [task("a", []), task("b", ["a"]), task("c", ["a"]), task("d", ["b", "c"])] };

    const { nodes, criticalPath } = buildDagView(dag, {});

    expect(criticalPath).toEqual(["a", "b", "d"]);
    const byId = Object.fromEntries(nodes.map((n) => [n.id, n]));
    expect(byId.b.row).toBe(0);
    expect(byId.c.row).toBe(1);
    expect(byId.b.onCriticalPath).toBe(true);
    expect(byId.c.onCriticalPath).toBe(false);
  });

  it("returns all-empty values for null dag and for an empty task list", () => {
    expect(buildDagView(null, {})).toEqual({ nodes: [], edges: [], criticalPath: [] });
    expect(buildDagView({ tasks: [] }, {})).toEqual({ nodes: [], edges: [], criticalPath: [] });
  });

  it("terminates and assigns layer 0 for a cycle A→B→A without crashing", () => {
    const dag: TaskDag = { tasks: [task("a", ["b"]), task("b", ["a"])] };

    const { nodes } = buildDagView(dag, {});

    const byId = Object.fromEntries(nodes.map((n) => [n.id, n]));
    expect(byId.a.layer).toBe(0);
    expect(byId.b.layer).toBe(0);
  });

  it("defaults a task with no taskStates entry to pending", () => {
    const dag: TaskDag = { tasks: [task("a", [])] };

    const { nodes } = buildDagView(dag, {});

    expect(nodes[0].state).toBe("pending");
  });

  it("falls back to an uppercase 2-letter code for roles not in the avatar map", () => {
    expect(avatarInitials("designer")).toBe("DS");
    expect(avatarInitials("unknown")).toBe("UN");
  });
});
