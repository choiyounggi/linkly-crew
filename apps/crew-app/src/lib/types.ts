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
  | "human.gate"
  | "human.response";

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
  | { type: "roster_changed"; agents: RosterAgentDto[]; ts: string }
  | PresenceEvent;

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

// ===== BEGIN MultiRunApi contract stub =====
// contract: t1-be-multirun owns the implementation
// run://event payload wrapper — every event is tagged with its run.
export interface RunEventEnvelope {
  run_id: string;
  event: RunEvent;
}
// list_runs() row — start_run(goal, scripted) -> run_id (unchanged);
// stop_run(run_id), run_snapshot(run_id), remove_run(run_id) take run_id.
export interface RunSummary {
  run_id: string;
  goal: string;
  finished: "completed" | "failed" | null;
}
// ===== END MultiRunApi contract stub =====

// ===== BEGIN PresenceEvent contract stub =====
// contract: t2-be-presence owns the implementation
// Volatile presence signal (never in ledger/snapshot messages).
export interface PresenceEvent {
  type: "presence";
  agent_id: string;
  kind: "read" | "typing";
  target_msg_id?: string;
  active?: boolean;
}
// ===== END PresenceEvent contract stub =====

// ===== BEGIN ProjectApi/OnboardingStatusApi contract stub =====
// contract: t3-be-project owns the implementation
// onboarding_status() row per supported tool.
export interface ToolStatus {
  id: string;
  installed: boolean;
  path: string | null;
  version: string | null;
  install_command: string;
  authenticated?: boolean; // gh only
}
// get_settings()/set_settings(settings)
export interface AppSettings {
  workspace_root: string;
}
// create_project(name) -> { name, path }
export interface ProjectInfo {
  name: string;
  path: string;
}
// ===== END ProjectApi/OnboardingStatusApi contract stub =====

// ===== BEGIN StartRunProjectRootArg/ListProjectsCommand contract stub (2차 런, 이슈 #13) =====
// contract: t1-be-projroot owns the Rust implementation (apps/crew-app/src-tauri/src/**);
// t2-fe-picker owns the TypeScript wiring (this file's consumers, source/store/modal).
// Both tasks build against the shapes declared here — neither redefines them.
// Full rationale: .orchestration/plans/t1-be-projroot/design.md "계약 요약".

/**
 * `invoke("start_run", args)`'s payload.
 *
 * `projectRoot` is ALWAYS sent explicitly, `null` included — never omit the key.
 * Tauri's docs do not state what an absent key does for an `Option<T>` argument
 * (only that arguments are passed as a JSON object with camelCase keys), so the
 * contract removes the dependency on that undocumented behavior instead of
 * relying on it.
 *
 * `null` keeps the pre-#13 behavior: every role's CLI cwd stays the per-role
 * scratch dir. A non-null value must be an absolute path (after `~` expansion)
 * naming a git repository's own toplevel — the backend rejects anything else
 * with an `Err` string rather than silently falling back to `null`.
 */
export interface StartRunArgs {
  goal: string;
  scripted: boolean;
  projectRoot: string | null;
}

/**
 * `invoke("list_projects")` — takes NO arguments; the backend reads the
 * workspace root from settings itself.
 *
 * Resolves to `ProjectInfo[]` sorted by `name` ascending, holding only the
 * directories directly under the workspace root that are git repositories.
 *
 * Rejects with `workspace_missing: <path>` when the workspace root is absent or
 * is not a directory. That is deliberately distinct from resolving to `[]`,
 * which means the root exists and simply holds no projects — a UI that collapses
 * the two reports a healthy "no projects" state over a misconfigured workspace.
 */
export type ListProjectsResult = ProjectInfo[];
// ===== END StartRunProjectRootArg/ListProjectsCommand contract stub =====
