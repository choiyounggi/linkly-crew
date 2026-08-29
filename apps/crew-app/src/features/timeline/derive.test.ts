import { describe, expect, it } from "vitest";

import { buildTimeline } from "./derive";
import type { Envelope } from "../../lib/types";

function envelope(
  overrides: Partial<Envelope> & Pick<Envelope, "id" | "kind" | "from" | "ts">,
): Envelope {
  return {
    sprint: "sprint-1",
    thread: "t-pm",
    to: ["x"],
    corr: "t-pm",
    in_reply_to: undefined,
    body: {},
    artifacts: [],
    requires_ack: false,
    deadline_ms: 1000,
    ...overrides,
  };
}

describe("buildTimeline — normal", () => {
  it("groups by role-ordered lane, sorts items by ts, and computes frac against the message range", () => {
    const assign = envelope({
      id: "e1",
      kind: "task.assign",
      from: "agent:developer",
      ts: "2024-01-01T00:00:00.000Z",
      body: { task: { id: "t1" } },
    });
    const changeRequest = envelope({
      id: "e2",
      kind: "change_request",
      from: "agent:developer",
      ts: "2024-01-01T00:00:05.000Z",
    });
    const result = envelope({
      id: "e3",
      kind: "task.result",
      from: "agent:pm",
      ts: "2024-01-01T00:00:10.000Z",
    });

    const { lanes, markers } = buildTimeline(
      [
        { seq: 1, envelope: assign },
        { seq: 2, envelope: changeRequest },
        { seq: 3, envelope: result },
      ],
      [
        { index: 0, startTs: "2024-01-01T00:00:00.000Z", endTs: null },
        { index: 1, startTs: "2024-01-01T00:00:20.000Z", endTs: null },
      ],
    );

    expect(lanes.map((l) => l.agentId)).toEqual(["pm", "developer"]);

    const developerLane = lanes.find((l) => l.agentId === "developer")!;
    expect(developerLane.items.map((i) => i.kind)).toEqual(["task.assign", "change_request"]);
    expect(developerLane.items[0].frac).toBe(0);
    expect(developerLane.items[0].label).toBe("task.assign t1");
    expect(developerLane.items[1].frac).toBe(0.5);

    const pmLane = lanes.find((l) => l.agentId === "pm")!;
    expect(pmLane.items[0].frac).toBe(1);

    expect(markers).toEqual([
      { index: 0, frac: 0 },
      { index: 1, frac: 1 }, // clamped: startTs falls past maxTs
    ]);
  });

  it("strips the 'agent:' prefix and orders unknown roles alphabetically after known ones", () => {
    const fromZeta = envelope({ id: "e1", kind: "question", from: "agent:zeta", ts: "2024-01-01T00:00:00.000Z" });
    const fromAlpha = envelope({ id: "e2", kind: "answer", from: "agent:alpha", ts: "2024-01-01T00:00:01.000Z" });
    const fromQa = envelope({ id: "e3", kind: "task.ack", from: "agent:qa", ts: "2024-01-01T00:00:02.000Z" });

    const { lanes } = buildTimeline(
      [
        { seq: 1, envelope: fromZeta },
        { seq: 2, envelope: fromAlpha },
        { seq: 3, envelope: fromQa },
      ],
      [],
    );

    expect(lanes.map((l) => l.agentId)).toEqual(["qa", "alpha", "zeta"]);
  });
});

describe("buildTimeline — boundary", () => {
  it("returns empty lanes and markers for no messages", () => {
    expect(buildTimeline([], [{ index: 0, startTs: "2024-01-01T00:00:00.000Z", endTs: null }])).toEqual({
      lanes: [],
      markers: [],
    });
  });

  it("collapses all fracs to 0 when every message shares a single ts (no division by zero)", () => {
    const a = envelope({ id: "e1", kind: "task.assign", from: "agent:pm", ts: "2024-01-01T00:00:00.000Z" });
    const b = envelope({ id: "e2", kind: "task.ack", from: "agent:pm", ts: "2024-01-01T00:00:00.000Z" });

    const { lanes, markers } = buildTimeline(
      [
        { seq: 1, envelope: a },
        { seq: 2, envelope: b },
      ],
      [{ index: 0, startTs: "2024-01-01T00:00:00.000Z", endTs: null }],
    );

    expect(lanes[0].items.map((i) => i.frac)).toEqual([0, 0]);
    expect(markers).toEqual([{ index: 0, frac: 0 }]);
  });

  it("excludes items and markers with an unparseable ts instead of crashing", () => {
    const good = envelope({ id: "e1", kind: "task.assign", from: "agent:pm", ts: "2024-01-01T00:00:00.000Z" });
    const bad = envelope({ id: "e2", kind: "task.result", from: "agent:pm", ts: "not-a-date" });

    const { lanes, markers } = buildTimeline(
      [
        { seq: 1, envelope: good },
        { seq: 2, envelope: bad },
      ],
      [
        { index: 0, startTs: "2024-01-01T00:00:00.000Z", endTs: null },
        { index: 1, startTs: "also-not-a-date", endTs: null },
      ],
    );

    expect(lanes).toHaveLength(1);
    expect(lanes[0].items).toHaveLength(1);
    expect(lanes[0].items[0].kind).toBe("task.assign");
    expect(markers).toEqual([{ index: 0, frac: 0 }]);
  });
});
