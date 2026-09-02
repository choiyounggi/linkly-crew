import { describe, expect, it } from "vitest";

import {
  parseChangeRequestBody,
  parseHumanGateBody,
  parseHumanResponseBody,
  parseTaskResultBody,
} from "./body";

describe("parseTaskResultBody — normal", () => {
  it("parses a well-formed task.result body", () => {
    const parsed = parseTaskResultBody({
      covered_req_ids: ["REQ-1", "REQ-2"],
      artifacts: [{ name: "spec.md", kind: "doc", req_ids: ["REQ-1"], content: "hello" }],
    });

    expect(parsed).toEqual({
      covered_req_ids: ["REQ-1", "REQ-2"],
      artifacts: [{ name: "spec.md", kind: "doc", req_ids: ["REQ-1"], content: "hello" }],
    });
  });
});

describe("parseTaskResultBody — boundary", () => {
  it("accepts an empty artifacts array", () => {
    expect(parseTaskResultBody({ covered_req_ids: [], artifacts: [] })).toEqual({
      covered_req_ids: [],
      artifacts: [],
    });
  });

  it("returns null for null/undefined body without throwing", () => {
    expect(parseTaskResultBody(null)).toBeNull();
    expect(parseTaskResultBody(undefined)).toBeNull();
  });
});

describe("parseTaskResultBody — error/malformed", () => {
  it("returns null when an artifact is missing content", () => {
    const parsed = parseTaskResultBody({
      covered_req_ids: ["REQ-1"],
      artifacts: [{ name: "spec.md" }],
    });

    expect(parsed).toBeNull();
  });
});

describe("parseChangeRequestBody — normal", () => {
  it("parses a well-formed change_request body", () => {
    expect(parseChangeRequestBody({ violations: ["REQ-2"], reason: "dod unmet" })).toEqual({
      violations: ["REQ-2"],
      reason: "dod unmet",
    });
  });
});

describe("parseChangeRequestBody — error/boundary", () => {
  it("returns null when reason is missing", () => {
    expect(parseChangeRequestBody({ violations: ["REQ-2"] })).toBeNull();
  });

  it("returns null for a non-object body", () => {
    expect(parseChangeRequestBody("nope")).toBeNull();
  });
});

describe("parseHumanGateBody — normal (D3)", () => {
  it("parses a well-formed human.gate body", () => {
    expect(parseHumanGateBody({ task_id: "t-qa", reason: "qa 차단 사유 검토 필요" })).toEqual({
      task_id: "t-qa",
      reason: "qa 차단 사유 검토 필요",
    });
  });
});

describe("parseHumanGateBody — error/boundary", () => {
  it("returns null when task_id is missing", () => {
    expect(parseHumanGateBody({ reason: "why" })).toBeNull();
  });

  it("returns null for an empty object", () => {
    expect(parseHumanGateBody({})).toBeNull();
  });
});

describe("parseHumanResponseBody — normal (D3)", () => {
  it("parses an approve response", () => {
    expect(parseHumanResponseBody({ task_id: "t-qa", decision: "approve", reason: "ok" })).toEqual({
      task_id: "t-qa",
      decision: "approve",
      reason: "ok",
    });
  });

  it("parses a reject response", () => {
    expect(parseHumanResponseBody({ task_id: "t-qa", decision: "reject", reason: "no" })).toEqual({
      task_id: "t-qa",
      decision: "reject",
      reason: "no",
    });
  });
});

describe("parseHumanResponseBody — error/boundary", () => {
  it("returns null for a decision outside approve/reject", () => {
    expect(parseHumanResponseBody({ task_id: "t-qa", decision: "maybe", reason: "x" })).toBeNull();
  });

  it("returns null for a non-object body", () => {
    expect(parseHumanResponseBody(42)).toBeNull();
  });
});
