import type { Envelope, Role, TaskDag, TaskStateDto } from "../../lib/types";

export type RailStatus = "working" | "awaiting" | "idle";

export interface AgentCard {
  id: string;
  role: Role | "lead";
  status: RailStatus;
  currentTaskId: string | null;
  harness: "claude";
}

type StoredMessage = { seq: number; envelope: Envelope };

const ROLES: Role[] = ["pm", "designer", "publisher", "developer", "qa"];

function taskIdFromAssignBody(body: unknown): string | null {
  if (typeof body !== "object" || body === null || !("task" in body)) return null;
  const task = (body as { task: unknown }).task;
  if (typeof task !== "object" || task === null || !("id" in task)) return null;
  const id = (task as { id: unknown }).id;
  return typeof id === "string" ? id : null;
}

/** Last (by message order) `task.assign` whose body.task.id belongs to a dag task of `role`. */
function findLastAssignForRole(
  role: Role,
  dag: TaskDag,
  messages: StoredMessage[],
): { taskId: string; corr: string; to: string[] } | null {
  const taskIdsForRole = new Set(dag.tasks.filter((t) => t.role === role).map((t) => t.id));
  let found: { taskId: string; corr: string; to: string[] } | null = null;
  for (const { envelope } of messages) {
    if (envelope.kind !== "task.assign") continue;
    const taskId = taskIdFromAssignBody(envelope.body);
    if (taskId === null || !taskIdsForRole.has(taskId)) continue;
    found = { taskId, corr: envelope.corr, to: envelope.to };
  }
  return found;
}

/** §C5: within the assign's corr, whether the last of {task.result, change_request} is task.result. */
function isAwaitingResult(corr: string, messages: StoredMessage[]): boolean {
  let awaiting = false;
  for (const { envelope } of messages) {
    if (envelope.corr !== corr) continue;
    if (envelope.kind === "task.result") awaiting = true;
    else if (envelope.kind === "change_request") awaiting = false;
  }
  return awaiting;
}

function roleCard(
  role: Role,
  dag: TaskDag,
  taskStates: Record<string, TaskStateDto>,
  messages: StoredMessage[],
): AgentCard {
  const assign = findLastAssignForRole(role, dag, messages);
  const fallbackId = `agent:${role}`;
  if (!assign) {
    return { id: fallbackId, role, status: "idle", currentTaskId: null, harness: "claude" };
  }

  const id = assign.to[0] ?? fallbackId;
  const state = taskStates[assign.taskId] ?? "pending";
  if (state !== "assigned") {
    return { id, role, status: "idle", currentTaskId: null, harness: "claude" };
  }

  const status: RailStatus = isAwaitingResult(assign.corr, messages) ? "awaiting" : "working";
  return { id, role, status, currentTaskId: assign.taskId, harness: "claude" };
}

/** D4: lead has no single current task — working while the sprint is in flight, idle otherwise. */
function leadCard(runId: string | null, finished: "completed" | "failed" | null): AgentCard {
  const status: RailStatus = runId !== null && finished === null ? "working" : "idle";
  return { id: "lead", role: "lead", status, currentTaskId: null, harness: "claude" };
}

/** Pure derivation per contracts-m4.md §C5 (rail cards). No store access. */
export function deriveRail(
  dag: TaskDag | null,
  taskStates: Record<string, TaskStateDto>,
  messages: StoredMessage[],
  runId: string | null,
  finished: "completed" | "failed" | null,
): AgentCard[] {
  const cards: AgentCard[] = [leadCard(runId, finished)];
  if (!dag) return cards;

  const rolesInDag = ROLES.filter((role) => dag.tasks.some((t) => t.role === role));
  for (const role of rolesInDag) {
    cards.push(roleCard(role, dag, taskStates, messages));
  }
  return cards;
}
