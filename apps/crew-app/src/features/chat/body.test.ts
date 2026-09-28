import { describe, expect, it } from "vitest";

import {
  parseBlockedBody,
  parseChangeRequestBody,
  parseHandoffBody,
  parseHumanGateBody,
  parseHumanResponseBody,
  parseTaskAssignBody,
  parseTaskProgressBody,
  parseTaskResultBody,
  parseTextBody,
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

describe("parseTaskAssignBody — normal", () => {
  it("parses a well-formed task.assign body", () => {
    expect(parseTaskAssignBody({ task: { title: "로그인 페이지 구현" } })).toEqual({
      title: "로그인 페이지 구현",
    });
  });
});

describe("parseTaskAssignBody — error/boundary", () => {
  it("returns null when task.title is missing", () => {
    expect(parseTaskAssignBody({ task: {} })).toBeNull();
  });

  it("returns null when task is not an object", () => {
    expect(parseTaskAssignBody({ task: "nope" })).toBeNull();
  });

  it("returns null for a non-object body", () => {
    expect(parseTaskAssignBody(null)).toBeNull();
  });
});

describe("parseTaskProgressBody — normal", () => {
  it("parses a well-formed task.progress body", () => {
    expect(parseTaskProgressBody({ summary: "구현 60% 진행" })).toEqual({
      summary: "구현 60% 진행",
    });
  });
});

describe("parseTaskProgressBody — error/boundary", () => {
  it("returns null when summary is missing", () => {
    expect(parseTaskProgressBody({})).toBeNull();
  });

  it("returns null for a non-object body", () => {
    expect(parseTaskProgressBody("nope")).toBeNull();
  });
});

describe("parseBlockedBody — normal", () => {
  it("parses a well-formed blocked body", () => {
    expect(parseBlockedBody({ reason: "의존성 대기 중" })).toEqual({ reason: "의존성 대기 중" });
  });
});

describe("parseBlockedBody — error/boundary", () => {
  it("returns null when reason is missing", () => {
    expect(parseBlockedBody({})).toBeNull();
  });

  it("returns null for a non-object body", () => {
    expect(parseBlockedBody(undefined)).toBeNull();
  });
});

describe("parseHandoffBody — normal", () => {
  it("parses a well-formed handoff body", () => {
    expect(parseHandoffBody({ pack: { role: "developer" } })).toEqual({ role: "developer" });
  });
});

describe("parseHandoffBody — error/boundary", () => {
  it("returns null when pack.role is missing", () => {
    expect(parseHandoffBody({ pack: {} })).toBeNull();
  });

  it("returns null when pack is not an object", () => {
    expect(parseHandoffBody({ pack: null })).toBeNull();
  });

  it("returns null for a non-object body", () => {
    expect(parseHandoffBody(42)).toBeNull();
  });
});

describe("parseTextBody — normal", () => {
  it("parses a well-formed text body", () => {
    expect(parseTextBody({ text: "REQ-4 반응형 기준이 뭔가요?" })).toEqual({
      text: "REQ-4 반응형 기준이 뭔가요?",
    });
  });
});

describe("parseTextBody — error/boundary", () => {
  it("returns null when text is missing", () => {
    expect(parseTextBody({})).toBeNull();
  });

  it("returns null for a non-object body", () => {
    expect(parseTextBody(null)).toBeNull();
  });
});
