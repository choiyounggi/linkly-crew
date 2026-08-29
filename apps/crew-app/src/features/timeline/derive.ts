import type { Envelope } from "../../lib/types";

export interface LaneItem {
  ts: string;
  kind: string;
  label: string;
  frac: number;
}

export interface Lane {
  agentId: string;
  items: LaneItem[];
}

export interface SprintMarker {
  index: number;
  frac: number;
}

type StoredMessage = { seq: number; envelope: Envelope };
type SprintWindow = { index: number; startTs: string; endTs: string | null };

/** 계약 §D5: 역할 우선순 — 그 외 from은 뒤에 사전순(compareLanes). */
const ROLE_ORDER = ["lead", "pm", "designer", "publisher", "developer", "qa"];

function agentIdFrom(from: string): string {
  return from.startsWith("agent:") ? from.slice("agent:".length) : from;
}

function laneRank(agentId: string): number {
  const rank = ROLE_ORDER.indexOf(agentId);
  return rank === -1 ? ROLE_ORDER.length : rank;
}

function compareLanes(a: string, b: string): number {
  const rankDiff = laneRank(a) - laneRank(b);
  return rankDiff !== 0 ? rankDiff : a.localeCompare(b);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** D5: task.assign 본문(body.task.id) 또는 body.task_id(그 외 kind)에서 방어적으로 추출. */
function taskIdFromBody(body: unknown): string | null {
  if (!isRecord(body)) return null;
  const task = body.task;
  if (isRecord(task) && typeof task.id === "string") return task.id;
  if (typeof body.task_id === "string") return body.task_id;
  return null;
}

function labelFor(envelope: Envelope): string {
  const taskId = taskIdFromBody(envelope.body);
  return taskId ? `${envelope.kind} ${taskId}` : envelope.kind;
}

/**
 * Pure derivation per contracts-m6.md §D5. `Date.parse` invalid ts → item/marker
 * excluded (never crash). span===0(단일 시각) → frac 전부 0.
 */
export function buildTimeline(
  messages: StoredMessage[],
  sprintWindows: SprintWindow[],
): { lanes: Lane[]; markers: SprintMarker[] } {
  const valid: { envelope: Envelope; parsedTs: number }[] = [];
  for (const { envelope } of messages) {
    const parsedTs = Date.parse(envelope.ts);
    if (!Number.isNaN(parsedTs)) valid.push({ envelope, parsedTs });
  }

  if (valid.length === 0) return { lanes: [], markers: [] };

  const minTs = Math.min(...valid.map((v) => v.parsedTs));
  const maxTs = Math.max(...valid.map((v) => v.parsedTs));
  const span = maxTs - minTs;

  const fracOf = (parsedTs: number): number => (span === 0 ? 0 : (parsedTs - minTs) / span);

  const byAgent = new Map<string, LaneItem[]>();
  for (const { envelope, parsedTs } of valid) {
    const agentId = agentIdFrom(envelope.from);
    const item: LaneItem = {
      ts: envelope.ts,
      kind: envelope.kind,
      label: labelFor(envelope),
      frac: fracOf(parsedTs),
    };
    const items = byAgent.get(agentId);
    if (items) items.push(item);
    else byAgent.set(agentId, [item]);
  }

  const lanes: Lane[] = Array.from(byAgent.entries())
    .sort(([a], [b]) => compareLanes(a, b))
    .map(([agentId, items]) => ({
      agentId,
      items: [...items].sort((x, y) => Date.parse(x.ts) - Date.parse(y.ts)),
    }));

  const markers: SprintMarker[] = [];
  for (const window of sprintWindows) {
    const parsedTs = Date.parse(window.startTs);
    if (Number.isNaN(parsedTs)) continue;
    markers.push({ index: window.index, frac: Math.min(1, Math.max(0, fracOf(parsedTs))) });
  }

  return { lanes, markers };
}
