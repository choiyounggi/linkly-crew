// Runtime guards for Envelope.body (typed `unknown` on the wire — D2).
// Malformed shapes return null; callers fall back to a raw JSON render.
// Never throw — a bad body must not crash the stream.

export interface Artifact {
  name: string;
  content: string;
  kind?: string;
  req_ids?: string[];
}

export interface TaskResultBody {
  covered_req_ids: string[];
  artifacts: Artifact[];
}

export interface ChangeRequestBody {
  violations: string[];
  reason: string;
}

export interface HumanGateBody {
  task_id: string;
  reason: string;
}

export interface HumanResponseBody {
  task_id: string;
  decision: "approve" | "reject";
  reason: string;
}

export interface TaskAssignBody {
  title: string;
}

export interface TaskProgressBody {
  summary: string;
}

export interface BlockedBody {
  reason: string;
}

export interface HandoffBody {
  role: string;
}

export interface TextBody {
  text: string;
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function parseArtifact(value: unknown): Artifact | null {
  if (!isRecord(value)) return null;
  if (typeof value.name !== "string" || typeof value.content !== "string") return null;
  const artifact: Artifact = { name: value.name, content: value.content };
  if (typeof value.kind === "string") artifact.kind = value.kind;
  if (isStringArray(value.req_ids)) artifact.req_ids = value.req_ids;
  return artifact;
}

/** contracts-m3.md: `{"covered_req_ids": [...], "artifacts": [{"name","kind","req_ids","content"}]}`. */
export function parseTaskResultBody(body: unknown): TaskResultBody | null {
  if (!isRecord(body)) return null;
  if (!isStringArray(body.covered_req_ids)) return null;
  if (!Array.isArray(body.artifacts)) return null;

  const artifacts: Artifact[] = [];
  for (const raw of body.artifacts) {
    const artifact = parseArtifact(raw);
    if (!artifact) return null;
    artifacts.push(artifact);
  }
  return { covered_req_ids: body.covered_req_ids, artifacts };
}

/** contracts-m3.md: `{"violations": [...], "reason": "..."}`. */
export function parseChangeRequestBody(body: unknown): ChangeRequestBody | null {
  if (!isRecord(body)) return null;
  if (!isStringArray(body.violations)) return null;
  if (typeof body.reason !== "string") return null;
  return { violations: body.violations, reason: body.reason };
}

/** contracts-m7.md §E8: `{"task_id": "...", "reason": "..."}`. */
export function parseHumanGateBody(body: unknown): HumanGateBody | null {
  if (!isRecord(body)) return null;
  if (typeof body.task_id !== "string") return null;
  if (typeof body.reason !== "string") return null;
  return { task_id: body.task_id, reason: body.reason };
}

/** mock-source's resolveGate response shape: `{"task_id","decision","reason"}`. */
export function parseHumanResponseBody(body: unknown): HumanResponseBody | null {
  if (!isRecord(body)) return null;
  if (typeof body.task_id !== "string") return null;
  if (body.decision !== "approve" && body.decision !== "reject") return null;
  if (typeof body.reason !== "string") return null;
  return { task_id: body.task_id, decision: body.decision, reason: body.reason };
}

/** DESIGN.md §3.2 `task.assign`: `{"task": {"title": "...", ...}}`. */
export function parseTaskAssignBody(body: unknown): TaskAssignBody | null {
  if (!isRecord(body)) return null;
  if (!isRecord(body.task)) return null;
  if (typeof body.task.title !== "string") return null;
  return { title: body.task.title };
}

/** DESIGN.md §3.2 `task.progress`: `{"summary": "..."}`. */
export function parseTaskProgressBody(body: unknown): TaskProgressBody | null {
  if (!isRecord(body)) return null;
  if (typeof body.summary !== "string") return null;
  return { summary: body.summary };
}

/** DESIGN.md §3.2 `blocked`: `{"reason": "..."}`. */
export function parseBlockedBody(body: unknown): BlockedBody | null {
  if (!isRecord(body)) return null;
  if (typeof body.reason !== "string") return null;
  return { reason: body.reason };
}

/** DESIGN.md §3.2 `handoff`: `{"pack": {"role": "...", ...}}`. */
export function parseHandoffBody(body: unknown): HandoffBody | null {
  if (!isRecord(body)) return null;
  if (!isRecord(body.pack)) return null;
  if (typeof body.pack.role !== "string") return null;
  return { role: body.pack.role };
}

/** DESIGN.md §3.2 `question`/`answer`: `{"text": "..."}` (shared shape). */
export function parseTextBody(body: unknown): TextBody | null {
  if (!isRecord(body)) return null;
  if (typeof body.text !== "string") return null;
  return { text: body.text };
}
