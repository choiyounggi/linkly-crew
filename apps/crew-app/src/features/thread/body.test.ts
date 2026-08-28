import { describe, expect, it } from "vitest";

import { parseChangeRequestBody, parseTaskResultBody } from "./body";

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

  it("returns null when covered_req_ids is not a string array", () => {
    expect(parseTaskResultBody({ covered_req_ids: "REQ-1", artifacts: [] })).toBeNull();
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
