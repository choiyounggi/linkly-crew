// Runtime guards for Envelope.body (typed `unknown` on the wire — D2).
// Malformed shapes return null; callers fall back to a raw JSON render.
// Never throw — a bad body must not crash the thread view.

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
