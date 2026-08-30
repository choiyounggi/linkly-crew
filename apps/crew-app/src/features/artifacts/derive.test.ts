import { describe, expect, it } from "vitest";

import type { Envelope, MessageKind, SpecDoc, TaskDag } from "../../lib/types";
import { buildArtifactIndex, buildReqMatrix, lineDiff } from "./derive";

function env(
  seq: number,
  fields: { kind: MessageKind; corr: string; body?: unknown },
): { seq: number; envelope: Envelope } {
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

function resultMsg(seq: number, taskId: string, coveredReqIds: string[], artifacts: { name: string; content: string }[]) {
  return env(seq, {
    kind: "task.result",
    corr: taskId,
    body: { covered_req_ids: coveredReqIds, artifacts },
  });
}

describe("buildArtifactIndex — version accumulation", () => {
  it("accumulates versions for the same (taskId, name) in seq order across repeated task.result", () => {
    const messages = [
      resultMsg(1, "t-a", [], [{ name: "spec.md", content: "v1" }]),
      resultMsg(3, "t-a", [], [{ name: "spec.md", content: "v2" }]),
    ];
    const index = buildArtifactIndex(messages);
    expect(index).toHaveLength(1);
    expect(index[0].taskId).toBe("t-a");
    expect(index[0].name).toBe("spec.md");
    expect(index[0].versions.map((v) => v.content)).toEqual(["v1", "v2"]);
    expect(index[0].versions.map((v) => v.msgSeq)).toEqual([1, 3]);
  });

  it("keeps distinct (taskId, name) pairs separate and ignores non task.result / unparseable messages", () => {
    const messages = [
      resultMsg(1, "t-a", [], [{ name: "spec.md", content: "a1" }]),
      resultMsg(2, "t-b", [], [{ name: "spec.md", content: "b1" }]),
      env(3, { kind: "task.assign", corr: "t-a", body: { task: { id: "t-a" } } }),
      env(4, { kind: "task.result", corr: "t-a", body: { garbage: true } }),
    ];
    const index = buildArtifactIndex(messages);
    expect(index).toHaveLength(2);
    expect(index.map((a) => `${a.taskId}/${a.name}`)).toEqual(["t-a/spec.md", "t-b/spec.md"]);
    expect(index[0].versions).toHaveLength(1);
  });

  it("returns an empty index for empty messages", () => {
    expect(buildArtifactIndex([])).toEqual([]);
  });
});

describe("lineDiff — accuracy", () => {
  it("marks every line as added when prev is null (first version)", () => {
    const diff = lineDiff(null, "a\nb");
    expect(diff).toEqual([
      { kind: "added", text: "a" },
      { kind: "added", text: "b" },
    ]);
  });

  it("marks a same/removed/added sequence for a single mid-line change", () => {
    const diff = lineDiff("a\nb", "a\nc");
    expect(diff).toEqual([
      { kind: "same", text: "a" },
      { kind: "removed", text: "b" },
      { kind: "added", text: "c" },
    ]);
  });

  it("marks every line same for identical content, with no added/removed", () => {
    const diff = lineDiff("a\nb\nc", "a\nb\nc");
    expect(diff.every((d) => d.kind === "same")).toBe(true);
    expect(diff).toHaveLength(3);
  });

  it("falls back to naive full removed+added when either side exceeds the LCS line limit", () => {
    const bigPrev = Array.from({ length: 2001 }, (_, i) => `l${i}`).join("\n");
    const diff = lineDiff(bigPrev, "x\ny");
    expect(diff.filter((d) => d.kind === "removed")).toHaveLength(2001);
    expect(diff.filter((d) => d.kind === "added")).toHaveLength(2);
  });
});

describe("buildReqMatrix — 3-state cells", () => {
  const SPEC: SpecDoc = {
    goal: "g",
    non_goals: [],
    constraints: [],
    requirements: [{ id: "REQ-1", text: "r1" }, { id: "REQ-2", text: "r2" }],
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
        artifacts_expected: [{ name: "spec.md", kind: "doc", req_ids: ["REQ-1", "REQ-2"] }],
      },
      {
        id: "t-b",
        role: "qa",
        title: "B",
        brief: "b",
        dod: [],
        deps: [],
        artifacts_expected: [],
      },
    ],
  };

  it("prioritizes covered over expected, and falls back to none with no expectation/coverage", () => {
    const messages = [resultMsg(1, "t-a", ["REQ-1"], [])];
    const matrix = buildReqMatrix(SPEC, DAG, messages);
    const req1 = matrix.rows.find((r) => r.reqId === "REQ-1")!;
    const req2 = matrix.rows.find((r) => r.reqId === "REQ-2")!;
    expect(req1.cells["t-a"]).toBe("covered");
    expect(req1.cells["t-b"]).toBe("none");
    expect(req2.cells["t-a"]).toBe("expected");
    expect(req2.cells["t-b"]).toBe("none");
  });

  it("uses only the latest (by seq) task.result per task for coverage — an earlier result doesn't stick", () => {
    const messages = [
      resultMsg(1, "t-a", ["REQ-1"], []),
      resultMsg(2, "t-a", [], []),
    ];
    const matrix = buildReqMatrix(SPEC, DAG, messages);
    const req1 = matrix.rows.find((r) => r.reqId === "REQ-1")!;
    expect(req1.cells["t-a"]).toBe("expected");
  });

  it("returns an empty matrix for null spec/dag and empty messages", () => {
    const matrix = buildReqMatrix(null, null, []);
    expect(matrix.rows).toEqual([]);
    expect(matrix.taskIds).toEqual([]);
  });
});
