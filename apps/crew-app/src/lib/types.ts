// TS mirror of crew-proto (Rust) wire types + crew-run's RunEvent (C3).
// Verbatim per contracts-m4.md §C3/§C4 — field names and shapes match the
// serde JSON 1:1. Do not rename fields; do not add fields not on the wire.

// --- crew-proto mirrors -----------------------------------------------

/** Rust `Role` — `#[serde(rename_all = "snake_case")]`. */
export type Role = "pm" | "designer" | "publisher" | "developer" | "qa";

/** Rust `ReqId` — wire representation is a plain "REQ-"-prefixed string. */
export type ReqId = string;

export interface Requirement {
  id: ReqId;
  text: string;
}

export interface SpecDoc {
  goal: string;
  non_goals: string[];
  constraints: string[];
  requirements: Requirement[];
  acceptance: string[];
}

export interface ArtifactContract {
  name: string;
  kind: string;
  req_ids: ReqId[];
}

/** Rust `DodCheck` — `#[serde(tag = "kind", rename_all = "snake_case")]`. */
export type DodCheck =
  | { kind: "cmd"; run: string; expect: string }
  | { kind: "req_cover"; ids: ReqId[] }
  | { kind: "browser"; flow: string; expect: string }
  | { kind: "artifact"; name: string };

export interface TaskSpec {
  id: string;
  role: Role;
  title: string;
  brief: string;
  dod: DodCheck[];
  deps: string[];
  artifacts_expected: ArtifactContract[];
}

export interface TaskDag {
  tasks: TaskSpec[];
}

/** Rust `MessageKind` — wire strings from DESIGN.md §3.2 (verbatim). */
export type MessageKind =
  | "task.assign"
  | "task.ack"
  | "task.progress"
  | "task.result"
  | "review.request"
  | "change_request"
  | "question"
  | "answer"
  | "blocked"
  | "handoff"
  | "human.gate";

/** Rust `Envelope` — DESIGN.md §3.1. */
export interface Envelope {
  id: string;
  ts: string;
  sprint: string;
  thread: string;
  from: string;
  to: string[];
  kind: MessageKind;
  in_reply_to?: string;
  corr: string;
  body: unknown;
  artifacts: string[];
  requires_ack: boolean;
  deadline_ms: number;
}

// --- crew-run RunEvent (C3) --------------------------------------------

export type TaskStateDto = "pending" | "assigned" | "accepted" | "escalated" | "blocked";

export type RunEvent =
  | { type: "run_started"; run_id: string; goal: string; ts: string }
  | { type: "spec_ready"; spec: SpecDoc; dag: TaskDag; sprint: string[]; ts: string }
  | { type: "message"; seq: number; envelope: Envelope }
  | { type: "task_state_changed"; task_id: string; state: TaskStateDto; ts: string }
  | { type: "bus_lifecycle"; seq: number; kind: string; payload: unknown }
  | { type: "run_finished"; outcome: "completed" | "failed"; ts: string }
  | { type: "sprint_started"; index: number; task_ids: string[]; ts: string }
  | { type: "sprint_finished"; index: number; summary: string; ts: string }
  | { type: "roster_changed"; agents: RosterAgentDto[]; ts: string };

// --- crew-run/crew-proto M5 roster mirrors (C7a) -----------------------

export interface RosterAgentDto {
  id: string;
  role: string;
  harness: string;
  model: string;
}

export interface HarnessInfo {
  id: string;
  installed: boolean;
  path: string | null;
  adapter: "real" | "stub" | "none";
}

export interface RosterAgent {
  id: string;
  role: string;
  harness: string;
  model: string;
  instructions: string;
}

export interface Roster {
  agents: RosterAgent[];
}

export interface RosterPreset {
  name: string;
  roster: Roster;
}
