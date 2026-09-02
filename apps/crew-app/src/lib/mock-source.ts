import type { RunEventSource } from "./source";
import type {
  Envelope,
  HarnessInfo,
  MessageKind,
  Requirement,
  Role,
  Roster,
  RosterAgent,
  RosterAgentDto,
  RosterPreset,
  RunEvent,
  SpecDoc,
  TaskDag,
  TaskSpec,
} from "./types";

// M3-shaped landing-page run (plan D9): fixed 5-role linear chain
// t-pm -> t-design -> t-publish -> t-dev -> t-qa, REQ-1..5, with one
// designer rework round trip (mirrors crew-lead's `plan_dag`/`LeadBehavior`
// shapes from contracts-m3.md / m3_sprint.rs so the mock reads like the
// real thing). M5 (plan D4) wraps this same 5-task chain into 3 sprints
// (sprint1=[t-pm,t-design], sprint2=[t-publish,t-dev], sprint3=[t-qa]) with
// a designer harness swap at the sprint1/sprint2 boundary.

const SPRINT_ID = "sprint-1";

const REQ_IDS = ["REQ-1", "REQ-2", "REQ-3", "REQ-4", "REQ-5"];

const REQUIREMENTS: Requirement[] = [
  { id: "REQ-1", text: "히어로 섹션(제목+한줄 소개)" },
  { id: "REQ-2", text: "핵심 기능 소개 3개 카드" },
  { id: "REQ-3", text: "CTA 버튼 1개" },
  { id: "REQ-4", text: "반응형 레이아웃(모바일 1열)" },
  { id: "REQ-5", text: "푸터(저작권 표기)" },
];

interface RoleTask {
  id: string;
  role: Role;
  agent: string;
  artifactName: string;
  artifactKind: string;
  deps: string[];
}

const ROLE_TASKS: RoleTask[] = [
  { id: "t-pm", role: "pm", agent: "agent:pm", artifactName: "spec.md", artifactKind: "doc", deps: [] },
  {
    id: "t-design",
    role: "designer",
    agent: "agent:designer",
    artifactName: "design.md",
    artifactKind: "doc",
    deps: ["t-pm"],
  },
  {
    id: "t-publish",
    role: "publisher",
    agent: "agent:publisher",
    artifactName: "index.html",
    artifactKind: "markup",
    deps: ["t-design"],
  },
  {
    id: "t-dev",
    role: "developer",
    agent: "agent:developer",
    artifactName: "app.js",
    artifactKind: "code",
    deps: ["t-publish"],
  },
  {
    id: "t-qa",
    role: "qa",
    agent: "agent:qa",
    artifactName: "qa-report.md",
    artifactKind: "report",
    deps: ["t-dev"],
  },
];

function buildSpecAndDag(goal: string): { spec: SpecDoc; dag: TaskDag } {
  const spec: SpecDoc = {
    goal,
    non_goals: [],
    constraints: [],
    requirements: REQUIREMENTS,
    acceptance: [],
  };
  const tasks: TaskSpec[] = ROLE_TASKS.map((t) => ({
    id: t.id,
    role: t.role,
    title: `${goal} — ${t.id}`,
    brief: `${goal} 요청에 대해 ${t.id} 작업을 수행한다`,
    dod: [{ kind: "req_cover", ids: REQ_IDS }],
    deps: t.deps,
    artifacts_expected: [{ name: t.artifactName, kind: t.artifactKind, req_ids: REQ_IDS }],
  }));
  return { spec, dag: { tasks } };
}

// --- roster (C7a / plan D5): lead + 5 roles, 6 slots fixed (contracts-m5 §C0) ---

const ROSTER_ROLES = ["lead", "pm", "designer", "publisher", "developer", "qa"] as const;

const DEFAULT_MODEL = "claude-sonnet-5";
const SAVINGS_MODEL = "claude-opus-5";

function makeAgentId(role: string): string {
  return `agent:${role}`;
}

function buildRoster(
  overrides: Partial<Record<(typeof ROSTER_ROLES)[number], Partial<Pick<RosterAgent, "harness" | "model">>>> = {},
): Roster {
  return {
    agents: ROSTER_ROLES.map((role) => ({
      id: makeAgentId(role),
      role,
      harness: overrides[role]?.harness ?? "claude-code",
      model: overrides[role]?.model ?? DEFAULT_MODEL,
      instructions: "",
    })),
  };
}

function rosterToDto(roster: Roster): RosterAgentDto[] {
  return roster.agents.map(({ id, role, harness, model }) => ({ id, role, harness, model }));
}

function withHarness(roster: Roster, role: string, harness: string): Roster {
  return { agents: roster.agents.map((a) => (a.role === role ? { ...a, harness } : a)) };
}

/** 내장 프리셋 3종 — 계약 C6 verbatim, 슬롯 6개 고정(배치만 다름). */
const PRESETS: RosterPreset[] = [
  { name: "클로드 5인팀", roster: buildRoster() },
  {
    name: "절약 모드",
    roster: buildRoster({ lead: { model: SAVINGS_MODEL }, developer: { model: SAVINGS_MODEL } }),
  },
  {
    name: "혼합 실험",
    roster: buildRoster({ publisher: { harness: "opencode", model: "default" } }),
  },
];

/** `HarnessRegistry::known()`(계약 C4b) 고정 목록의 인메모리 미러 (plan D5). */
const KNOWN_HARNESSES: HarnessInfo[] = [
  { id: "claude-code", installed: true, path: "/usr/local/bin/claude", adapter: "real" },
  { id: "codex", installed: false, path: null, adapter: "none" },
  { id: "gemini", installed: false, path: null, adapter: "none" },
  { id: "grok", installed: false, path: null, adapter: "none" },
  { id: "opencode", installed: true, path: "/usr/local/bin/opencode", adapter: "stub" },
  { id: "ollama", installed: false, path: null, adapter: "none" },
];

// --- scenario ------------------------------------------------------------

// Placeholder for a RunEvent/Envelope's `ts` at build time. `buildScenario`
// runs synchronously in one burst, so any timestamp stamped here would be
// (near-)identical across all events; `restampWithNow` overwrites this
// right before each event is actually delivered, so `ts` reflects the
// moment of delivery instead (review r1 F1 / HANDOFF pitfall 12).
const PENDING_TS = "";

interface SprintPlan {
  index: number;
  taskIds: string[];
  summary: string;
}

function buildSprintPlans(): SprintPlan[] {
  return [
    { index: 1, taskIds: ["t-pm", "t-design"], summary: "스프린트 1 완료: t-pm, t-design 수락" },
    { index: 2, taskIds: ["t-publish", "t-dev"], summary: "스프린트 2 완료: t-publish, t-dev 수락" },
    { index: 3, taskIds: ["t-qa"], summary: "스프린트 3: t-qa 에스컬레이션(승인 대기)" },
  ];
}

function buildScenario(goal: string, initialRoster: Roster, allocateSeq: () => number): RunEvent[] {
  const events: RunEvent[] = [];
  let envCounter = 0;
  const nextEnvId = () => `env_${++envCounter}`;

  events.push({ type: "run_started", run_id: "run_mock", goal, ts: PENDING_TS });
  events.push({ type: "roster_changed", agents: rosterToDto(initialRoster), ts: PENDING_TS });

  const { spec, dag } = buildSpecAndDag(goal);
  const sprint = ROLE_TASKS.map((t) => t.id);
  events.push({ type: "spec_ready", spec, dag, sprint, ts: PENDING_TS });

  const makeEnvelope = (fields: {
    thread: string;
    from: string;
    to: string[];
    kind: MessageKind;
    corr: string;
    body: unknown;
    in_reply_to?: string;
  }): Envelope => ({
    id: nextEnvId(),
    ts: PENDING_TS,
    sprint: SPRINT_ID,
    thread: fields.thread,
    from: fields.from,
    to: fields.to,
    kind: fields.kind,
    in_reply_to: fields.in_reply_to,
    corr: fields.corr,
    body: fields.body,
    artifacts: [],
    requires_ack: false,
    deadline_ms: 60_000,
  });

  const pushMessage = (envelope: Envelope) => {
    events.push({ type: "message", seq: allocateSeq(), envelope });
  };

  const runTask = (task: TaskSpec) => {
    const roleTask = ROLE_TASKS.find((t) => t.id === task.id);
    if (!roleTask) return;

    const assign = makeEnvelope({
      thread: task.id,
      from: "lead",
      to: [roleTask.agent],
      kind: "task.assign",
      corr: task.id,
      body: { task },
    });
    pushMessage(assign);
    events.push({ type: "task_state_changed", task_id: task.id, state: "assigned", ts: PENDING_TS });

    const ack = makeEnvelope({
      thread: task.id,
      from: roleTask.agent,
      to: ["lead"],
      kind: "task.ack",
      corr: task.id,
      in_reply_to: assign.id,
      body: {},
    });
    pushMessage(ack);

    if (roleTask.role === "qa") {
      // Escalation demo (contracts-m7.md §E8/plan D5): qa reports blocked,
      // lead escalates via human.gate, task lands on "escalated" (not
      // "accepted") — the run still reaches run_finished(completed) despite
      // this (existing M3 completion semantics), and resolveGate()/the
      // inbox feature act on it live from there.
      const blocked = makeEnvelope({
        thread: task.id,
        from: roleTask.agent,
        to: ["lead"],
        kind: "blocked",
        corr: task.id,
        in_reply_to: ack.id,
        body: { reason: "REQ-4 반응형 레이아웃을 재현할 수 없음 — 승인 필요" },
      });
      pushMessage(blocked);
      events.push({ type: "task_state_changed", task_id: task.id, state: "blocked", ts: PENDING_TS });

      const gate = makeEnvelope({
        thread: task.id,
        from: "lead",
        to: [],
        kind: "human.gate",
        corr: task.id,
        in_reply_to: blocked.id,
        body: { task_id: task.id, reason: "qa 차단 사유 검토 필요 — 승인/반려 결정 대기" },
      });
      pushMessage(gate);
      events.push({ type: "task_state_changed", task_id: task.id, state: "escalated", ts: PENDING_TS });
      return;
    }

    if (roleTask.role === "designer") {
      const badResult = makeEnvelope({
        thread: task.id,
        from: roleTask.agent,
        to: ["lead"],
        kind: "task.result",
        corr: task.id,
        in_reply_to: ack.id,
        body: {
          covered_req_ids: REQ_IDS.filter((id) => id !== "REQ-2"),
          artifacts: [{ name: roleTask.artifactName, content: "(초안)" }],
        },
      });
      pushMessage(badResult);

      const changeRequest = makeEnvelope({
        thread: task.id,
        from: "lead",
        to: [roleTask.agent],
        kind: "change_request",
        corr: task.id,
        in_reply_to: badResult.id,
        body: { violations: ["REQ-2"], reason: "dod unmet" },
      });
      pushMessage(changeRequest);

      const fixedResult = makeEnvelope({
        thread: task.id,
        from: roleTask.agent,
        to: ["lead"],
        kind: "task.result",
        corr: task.id,
        in_reply_to: changeRequest.id,
        body: {
          covered_req_ids: REQ_IDS,
          artifacts: [{ name: roleTask.artifactName, content: "(수정 완료)" }],
        },
      });
      pushMessage(fixedResult);
    } else {
      const result = makeEnvelope({
        thread: task.id,
        from: roleTask.agent,
        to: ["lead"],
        kind: "task.result",
        corr: task.id,
        in_reply_to: ack.id,
        body: {
          covered_req_ids: REQ_IDS,
          artifacts: [{ name: roleTask.artifactName, content: "(완료)" }],
        },
      });
      pushMessage(result);
    }

    events.push({ type: "task_state_changed", task_id: task.id, state: "accepted", ts: PENDING_TS });
  };

  let swappedRoster = initialRoster;

  for (const plan of buildSprintPlans()) {
    events.push({ type: "sprint_started", index: plan.index, task_ids: plan.taskIds, ts: PENDING_TS });

    for (const taskId of plan.taskIds) {
      const task = dag.tasks.find((t) => t.id === taskId);
      if (task) runTask(task);
    }

    events.push({ type: "sprint_finished", index: plan.index, summary: plan.summary, ts: PENDING_TS });

    if (plan.index === 1) {
      // 스프린트 2 시작 전 designer 하네스 스왑 (plan D4) — 핸드오프 메시지 +
      // roster_changed를 스프린트 경계에 박아 넣은 고정 시나리오. (별개로
      // `swapHarness()`가 즉시 실행하는 라이브 스왑도 제공한다 — plan D5.)
      swappedRoster = withHarness(swappedRoster, "designer", "opencode");
      const handoff = makeEnvelope({
        thread: "t-design",
        from: "lead",
        to: ["agent:designer"],
        kind: "handoff",
        corr: "t-design",
        body: {
          pack: {
            role: "designer",
            spec_ref: goal,
            done: ["t-pm", "t-design"],
            in_flight: [],
            decisions: [],
            open_questions: [],
            notes: "하네스 교체: claude-code → opencode",
          },
        },
      });
      pushMessage(handoff);
      events.push({ type: "roster_changed", agents: rosterToDto(swappedRoster), ts: PENDING_TS });
    }
  }

  events.push({ type: "run_finished", outcome: "completed", ts: PENDING_TS });
  return events;
}

/**
 * Replaces an event's `PENDING_TS` placeholder(s) with the current instant.
 * `message` events carry their timestamp on `envelope.ts`; every other
 * variant (including `sprint_started`/`sprint_finished`/`roster_changed`)
 * carries a top-level `ts`. Called right before delivery so timestamps
 * progress across the replay instead of freezing at build time (review r1
 * F1).
 */
function restampWithNow(ev: RunEvent): RunEvent {
  const now = new Date().toISOString();
  switch (ev.type) {
    case "message":
      return { ...ev, envelope: { ...ev.envelope, ts: now } };
    case "bus_lifecycle":
      // No top-level `ts` on this variant, and the mock never emits it.
      return ev;
    case "presence":
      // No `ts` field on this variant either (t2-be-presence D3: a
      // volatile signal, not restamped like the ledgered types below), and
      // the mock never emits it.
      return ev;
    default:
      return { ...ev, ts: now };
  }
}

/**
 * Demo/test default source (plan D9): replays a scripted M3-shaped,
 * 3-sprint run (plan D4) on timers, `intervalMs` apart (default 300; tests
 * pass 0 with fake timers). Also implements the C7a optional roster/harness
 * methods (plan D5) as an in-memory simulation.
 */
export class MockEventSource implements RunEventSource {
  private readonly intervalMs: number;
  private readonly listeners = new Set<(ev: RunEvent) => void>();
  private timers: ReturnType<typeof setTimeout>[] = [];
  private roster: Roster;
  private nextSeq = 1;
  private swapEnvCounter = 0;
  private gateEnvCounter = 0;
  /** Messages actually delivered so far (plan D4) — `searchMessages` filters over this, not the full scripted scenario. */
  private deliveredMessages: { seq: number; envelope: Envelope }[] = [];

  constructor(intervalMs = 300, roster: Roster = buildRoster()) {
    this.intervalMs = intervalMs;
    this.roster = roster;
  }

  async start(goal: string): Promise<void> {
    this.clearTimers();
    this.nextSeq = 1;
    this.deliveredMessages = [];
    const initialRoster = this.roster;
    const events = buildScenario(goal, initialRoster, () => this.nextSeq++);
    // The scripted scenario always ends with the designer swapped to
    // opencode (plan D4); reflect that on the instance roster right away
    // rather than waiting for the delayed roster_changed to actually
    // deliver, so getRoster() is consistent with "a run was started" even
    // before its timers finish (simplification — acceptable for a mock).
    this.roster = withHarness(initialRoster, "designer", "opencode");

    events.forEach((ev, index) => {
      const timer = setTimeout(() => {
        this.emit(restampWithNow(ev));
      }, index * this.intervalMs);
      this.timers.push(timer);
    });
  }

  onEvent(cb: (ev: RunEvent) => void): () => void {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  }

  async stop(): Promise<void> {
    this.clearTimers();
  }

  private clearTimers(): void {
    for (const timer of this.timers) clearTimeout(timer);
    this.timers = [];
  }

  /** Delivers one event to subscribers, tracking `message`s for `searchMessages` (plan D4). */
  private emit(ev: RunEvent): void {
    if (ev.type === "message") {
      this.deliveredMessages.push({ seq: ev.seq, envelope: ev.envelope });
    }
    for (const cb of this.listeners) cb(ev);
  }

  // --- C7a optional methods (plan D5) -------------------------------------

  /** In-memory swap: updates the roster, then replays handoff + roster_changed immediately. */
  async swapHarness(agentId: string, harness: string): Promise<void> {
    const target = this.roster.agents.find((a) => a.id === agentId);
    if (!target) {
      throw new Error(`swapHarness: unknown agent id "${agentId}"`);
    }

    const previousHarness = target.harness;
    this.roster = { agents: this.roster.agents.map((a) => (a.id === agentId ? { ...a, harness } : a)) };
    const now = new Date().toISOString();

    const handoffEnvelope: Envelope = {
      id: `env_swap_${++this.swapEnvCounter}`,
      ts: now,
      sprint: SPRINT_ID,
      thread: target.role,
      from: "lead",
      to: [agentId],
      kind: "handoff",
      corr: target.role,
      body: {
        pack: {
          role: target.role,
          spec_ref: "",
          done: [],
          in_flight: [],
          decisions: [],
          open_questions: [],
          notes: `하네스 교체: ${previousHarness} → ${harness}`,
        },
      },
      artifacts: [],
      requires_ack: false,
      deadline_ms: 60_000,
    };

    const handoffEvent: RunEvent = { type: "message", seq: this.nextSeq++, envelope: handoffEnvelope };
    const rosterEvent: RunEvent = { type: "roster_changed", agents: rosterToDto(this.roster), ts: now };

    this.emit(handoffEvent);
    this.emit(rosterEvent);
  }

  async getRoster(): Promise<Roster> {
    return this.roster;
  }

  async setRoster(roster: Roster): Promise<void> {
    this.roster = roster;
  }

  async listPresets(): Promise<RosterPreset[]> {
    return PRESETS;
  }

  async detectHarnesses(): Promise<HarnessInfo[]> {
    return KNOWN_HARNESSES;
  }

  // --- E8 gate/search methods (plan D4) -----------------------------------

  /**
   * Replays a human.response message, then the resulting state transition:
   * "approve" -> assigned -> accepted; "reject" -> blocked (contracts-m7.md §E8).
   */
  async resolveGate(taskId: string, decision: "approve" | "reject", reason: string): Promise<void> {
    if (!ROLE_TASKS.some((t) => t.id === taskId)) {
      throw new Error(`resolveGate: unknown task id "${taskId}"`);
    }

    const now = new Date().toISOString();
    const responseEnvelope: Envelope = {
      id: `env_gate_${++this.gateEnvCounter}`,
      ts: now,
      sprint: SPRINT_ID,
      thread: taskId,
      from: "human",
      to: ["lead"],
      kind: "human.response",
      corr: taskId,
      body: { task_id: taskId, decision, reason },
      artifacts: [],
      requires_ack: false,
      deadline_ms: 60_000,
    };
    this.emit({ type: "message", seq: this.nextSeq++, envelope: responseEnvelope });

    if (decision === "approve") {
      this.emit({ type: "task_state_changed", task_id: taskId, state: "assigned", ts: now });
      this.emit({ type: "task_state_changed", task_id: taskId, state: "accepted", ts: now });
    } else {
      this.emit({ type: "task_state_changed", task_id: taskId, state: "blocked", ts: now });
    }
  }

  /** In-memory, case-insensitive substring filter over kind/from/body of delivered messages (plan D4). */
  async searchMessages(query: string): Promise<{ seq: number; envelope: Envelope }[]> {
    const needle = query.toLowerCase();
    return this.deliveredMessages.filter(({ envelope }) => {
      const haystack = `${envelope.kind} ${envelope.from} ${JSON.stringify(envelope.body ?? "")}`.toLowerCase();
      return haystack.includes(needle);
    });
  }
}
