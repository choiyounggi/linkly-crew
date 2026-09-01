import { useRunStore } from "../../lib/store";
import type { Role } from "../../lib/types";
import { avatarInitials, deriveBoard, type BoardCard, type BoardColumns } from "./derive";
import "./board.css";

const COLUMNS: { key: keyof BoardColumns; label: string }[] = [
  { key: "pending", label: "대기" },
  { key: "assigned", label: "진행" },
  { key: "review", label: "검토" },
  { key: "accepted", label: "완료" },
  { key: "escalated", label: "차단" },
];

const ROLE_CLASS: Record<Role, string> = {
  pm: "avatar--pm",
  designer: "avatar--designer",
  publisher: "avatar--publisher",
  developer: "avatar--developer",
  qa: "avatar--qa",
};

function RoleAvatar({ role }: { role: Role }) {
  return (
    <span className={`board-avatar ${ROLE_CLASS[role]}`} title={role}>
      {avatarInitials(role)}
    </span>
  );
}

function Card({ card }: { card: BoardCard }) {
  return (
    <li className="board-card">
      <RoleAvatar role={card.task.role} />
      <span className="board-card__title">{card.task.title}</span>
      {card.blockReason && <span className="board-card__block-label">{card.blockReason}</span>}
      {card.reworkCount > 0 && (
        <span className="board-card__badge" aria-label={`리워크 ${card.reworkCount}회`}>
          ↺{card.reworkCount}
        </span>
      )}
    </li>
  );
}

export default function Board() {
  const dag = useRunStore((s) => s.dag);
  const taskStates = useRunStore((s) => s.taskStates);
  const messages = useRunStore((s) => s.messages);
  const board = deriveBoard(dag, taskStates, messages);

  return (
    <section className="panel panel--board" aria-label="스프린트 보드">
      <h2 className="panel__title">스프린트 보드</h2>
      <div className="board-columns">
        {COLUMNS.map(({ key, label }) => (
          <section key={key} className="board-column" aria-label={label}>
            <h3 className="board-column__title">
              <span>{label}</span>
              <span className="board-column__count">{board[key].length}</span>
            </h3>
            <ul className="board-column__list">
              {board[key].map((card) => (
                <Card key={card.task.id} card={card} />
              ))}
            </ul>
          </section>
        ))}
      </div>
    </section>
  );
}
