import { useRunStore } from "../../lib/store";
import { avatarInitials, deriveRail, type AgentCard, type RailStatus } from "./derive";
import "./rail.css";

const STATUS_LABEL: Record<RailStatus, string> = {
  working: "작업",
  awaiting: "응답대기",
  idle: "대기",
};

const ROLE_LABEL: Record<AgentCard["role"], string> = {
  lead: "Lead",
  pm: "PM",
  designer: "Designer",
  publisher: "Publisher",
  developer: "Developer",
  qa: "QA",
};

function Card({ card }: { card: AgentCard }) {
  return (
    <li className="rail-card">
      <div className="rail-card__header">
        <span className="rail-card__avatar" aria-hidden="true">
          {avatarInitials(card.role)}
        </span>
        <span className="rail-card__name">{ROLE_LABEL[card.role]}</span>
        <span className={`rail-badge rail-badge--${card.status}`}>{STATUS_LABEL[card.status]}</span>
      </div>
      <span className="rail-card__task">{card.currentTaskId ?? "—"}</span>
      <span className="rail-card__harness">{card.harness}</span>
    </li>
  );
}

export default function Rail() {
  const dag = useRunStore((s) => s.dag);
  const taskStates = useRunStore((s) => s.taskStates);
  const messages = useRunStore((s) => s.messages);
  const runId = useRunStore((s) => s.runId);
  const finished = useRunStore((s) => s.finished);
  const roster = useRunStore((s) => s.roster);
  const cards = deriveRail(dag, taskStates, messages, runId, finished, roster);

  return (
    <section className="panel panel--rail" aria-label="에이전트 레일">
      <h2 className="panel__title">에이전트 레일</h2>
      <ul className="rail-list">
        {cards.map((card) => (
          <Card key={card.id} card={card} />
        ))}
      </ul>
    </section>
  );
}
