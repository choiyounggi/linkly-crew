import type { Envelope, TaskDag, TaskSpec, TaskStateDto } from "../../lib/types";

export interface BoardCard {
  task: TaskSpec;
  reworkCount: number;
}

export interface BoardColumns {
  pending: BoardCard[];
  assigned: BoardCard[];
  review: BoardCard[];
  accepted: BoardCard[];
  escalated: BoardCard[];
}

type StoredMessage = { seq: number; envelope: Envelope };

function isAssignBodyForTask(body: unknown, taskId: string): boolean {
  if (typeof body !== "object" || body === null || !("task" in body)) return false;
  const task = (body as { task: unknown }).task;
  if (typeof task !== "object" || task === null || !("id" in task)) return false;
  return (task as { id: unknown }).id === taskId;
}

/** Last (by seq) `task.assign` addressed to `taskId`, guarded on body shape (contract §M3: body={"task":TaskSpec}). */
function findLastAssignCorr(taskId: string, messages: StoredMessage[]): string | null {
  let corr: string | null = null;
  for (const { envelope } of messages) {
    if (envelope.kind === "task.assign" && isAssignBodyForTask(envelope.body, taskId)) {
      corr = envelope.corr;
    }
  }
  return corr;
}

/** §C5 D2: within the assign's corr, whether the last of {task.result, change_request} is task.result. */
function isAwaitingReview(corr: string, messages: StoredMessage[]): boolean {
  let awaiting = false;
  for (const { envelope } of messages) {
    if (envelope.corr !== corr) continue;
    if (envelope.kind === "task.result") awaiting = true;
    else if (envelope.kind === "change_request") awaiting = false;
  }
  return awaiting;
}

function countChangeRequests(corr: string, messages: StoredMessage[]): number {
  return messages.filter(({ envelope }) => envelope.corr === corr && envelope.kind === "change_request").length;
}

function emptyColumns(): BoardColumns {
  return { pending: [], assigned: [], review: [], accepted: [], escalated: [] };
}

/** Pure derivation per contracts-m4.md §C5 (board columns). No store access, no memoization (D4). */
export function deriveBoard(
  dag: TaskDag | null,
  taskStates: Record<string, TaskStateDto>,
  messages: StoredMessage[],
): BoardColumns {
  const columns = emptyColumns();
  if (!dag) return columns;

  for (const task of dag.tasks) {
    const state: TaskStateDto = taskStates[task.id] ?? "pending";
    const corr = findLastAssignCorr(task.id, messages);
    const reworkCount = corr === null ? 0 : countChangeRequests(corr, messages);
    const card: BoardCard = { task, reworkCount };

    switch (state) {
      case "pending":
        columns.pending.push(card);
        break;
      case "accepted":
        columns.accepted.push(card);
        break;
      case "escalated":
        columns.escalated.push(card);
        break;
      case "assigned":
        if (corr !== null && isAwaitingReview(corr, messages)) columns.review.push(card);
        else columns.assigned.push(card);
        break;
    }
  }

  return columns;
}
