import { describe, expect, it } from "vitest";

import { fallbackSearch, snippet } from "./derive";
import type { Envelope } from "../../lib/types";

function envelope(overrides: Partial<Envelope> & Pick<Envelope, "id" | "kind" | "from" | "corr">): Envelope {
  return {
    ts: "2026-08-30T00:00:00.000Z",
    sprint: "sprint-1",
    thread: "t-pm",
    to: ["x"],
    in_reply_to: undefined,
    body: {},
    artifacts: [],
    requires_ack: false,
    deadline_ms: 1000,
    ...overrides,
  };
}

describe("fallbackSearch — normal", () => {
  it("matches a query found in the body, case-insensitively", () => {
    const target = envelope({ id: "e1", kind: "task.result", from: "agent:pm", corr: "t-pm", body: { text: "Deploy Succeeded" } });
    const other = envelope({ id: "e2", kind: "task.result", from: "agent:qa", corr: "t-qa", body: { text: "unrelated" } });

    const results = fallbackSearch(
      [
        { seq: 1, envelope: target },
        { seq: 2, envelope: other },
      ],
      "deploy succ",
    );

    expect(results).toEqual([{ seq: 1, envelope: target }]);
  });

  it("matches against kind and from, not just body", () => {
    const byFrom = envelope({ id: "e1", kind: "task.assign", from: "agent:designer", corr: "t-designer", body: {} });

    const results = fallbackSearch([{ seq: 1, envelope: byFrom }], "designer");

    expect(results).toEqual([{ seq: 1, envelope: byFrom }]);
  });
});

describe("fallbackSearch — edge cases", () => {
  it("returns an empty array when nothing matches", () => {
    const only = envelope({ id: "e1", kind: "task.result", from: "agent:pm", corr: "t-pm", body: { text: "hello" } });

    const results = fallbackSearch([{ seq: 1, envelope: only }], "nonexistent-query");

    expect(results).toEqual([]);
  });

  it("returns an empty array for an empty message list", () => {
    expect(fallbackSearch([], "anything")).toEqual([]);
  });
});

describe("snippet — boundary", () => {
  it("returns the full text without ellipsis when exactly 120 characters", () => {
    const text = "x".repeat(120);
    const env = envelope({ id: "e1", kind: "task.result", from: "agent:pm", corr: "t-pm", body: text });

    expect(snippet(env)).toBe(text);
    expect(snippet(env)).toHaveLength(120);
  });

  it("truncates to 120 characters and appends an ellipsis when longer", () => {
    const text = "x".repeat(121);
    const env = envelope({ id: "e1", kind: "task.result", from: "agent:pm", corr: "t-pm", body: text });

    const result = snippet(env);

    expect(result).toBe(`${"x".repeat(120)}…`);
    expect(result).toHaveLength(121);
  });

  it("stringifies a non-string body without truncation when short", () => {
    const env = envelope({ id: "e1", kind: "task.result", from: "agent:pm", corr: "t-pm", body: { a: 1 } });

    expect(snippet(env)).toBe(JSON.stringify({ a: 1 }));
  });
});
