import type { RunEventSource } from "./source";
import type {
  Envelope,
  MessageKind,
  Requirement,
  Role,
  RunEvent,
  SpecDoc,
  TaskDag,
  TaskSpec,
} from "./types";

// M3-shaped landing-page run (plan D9): fixed 5-role linear chain
// t-pm -> t-design -> t-publish -> t-dev -> t-qa, REQ-1..5, with one
// designer rework round trip (mirrors crew-lead's `plan_dag`/`LeadBehavior`
// shapes from contracts-m3.md / m3_sprint.rs so the mock reads like the
// real thing).

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

function buildScenario(goal: string): RunEvent[] {
  const events: RunEvent[] = [];
  let seq = 0;
  let envCounter = 0;
  const nextEnvId = () => `env_${++envCounter}`;
  const ts = () => new Date().toISOString();

  events.push({ type: "run_started", run_id: "run_mock", goal, ts: ts() });

  const { spec, dag } = buildSpecAndDag(goal);
  const sprint = ROLE_TASKS.map((t) => t.id);
  events.push({ type: "spec_ready", spec, dag, sprint, ts: ts() });

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
    ts: ts(),
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
    seq += 1;
    events.push({ type: "message", seq, envelope });
  };

  for (const task of dag.tasks) {
    const roleTask = ROLE_TASKS.find((t) => t.id === task.id);
    if (!roleTask) continue;

    const assign = makeEnvelope({
      thread: task.id,
      from: "lead",
      to: [roleTask.agent],
      kind: "task.assign",
      corr: task.id,
      body: { task },
    });
    pushMessage(assign);
    events.push({ type: "task_state_changed", task_id: task.id, state: "assigned", ts: ts() });

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

    events.push({ type: "task_state_changed", task_id: task.id, state: "accepted", ts: ts() });
  }

  events.push({ type: "run_finished", outcome: "completed", ts: ts() });
  return events;
}

/**
 * Demo/test default source (plan D9): replays a scripted M3-shaped run on
 * timers, `intervalMs` apart (default 300; tests pass 0 with fake timers).
 */
export class MockEventSource implements RunEventSource {
  private readonly intervalMs: number;
  private readonly listeners = new Set<(ev: RunEvent) => void>();
  private timers: ReturnType<typeof setTimeout>[] = [];

  constructor(intervalMs = 300) {
    this.intervalMs = intervalMs;
  }

  async start(goal: string): Promise<void> {
    this.clearTimers();
    const events = buildScenario(goal);
    events.forEach((ev, index) => {
      const timer = setTimeout(() => {
        for (const cb of this.listeners) cb(ev);
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
}
