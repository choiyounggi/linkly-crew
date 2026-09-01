import { useEffect, useState, type FormEvent } from "react";

import Artifacts from "./features/artifacts";
import Board from "./features/board";
import DagView from "./features/dag";
import Inbox from "./features/inbox";
import Rail from "./features/rail";
import RosterPanel from "./features/roster";
import SearchBox from "./features/search";
import Thread from "./features/thread";
import TimelineView from "./features/timeline";
import { Button } from "./components/primitives";
import { defaultSource, useRunStore } from "./lib/store";
import "./App.css";

type View = "board" | "dag" | "timeline" | "inbox" | "artifacts";

const TABS: { key: View; label: string }[] = [
  { key: "board", label: "보드" },
  { key: "dag", label: "DAG" },
  { key: "timeline", label: "타임라인" },
  { key: "inbox", label: "승인함" },
  { key: "artifacts", label: "아티팩트" },
];

export default function App() {
  const [goalInput, setGoalInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<View>("board");
  const applyEvent = useRunStore((s) => s.applyEvent);
  const startRun = useRunStore((s) => s.startRun);
  const runId = useRunStore((s) => s.runId);
  const finished = useRunStore((s) => s.finished);
  const isRunning = runId !== null && finished === null;

  // D5: one effect wires the subscription; applying the event is the
  // store's job (applyEvent), not the effect's.
  useEffect(() => {
    return defaultSource.onEvent(applyEvent);
  }, [applyEvent]);

  const submitGoal = () => {
    const goal = goalInput.trim();
    if (!goal) {
      setError("요청을 입력하세요");
      return;
    }
    setError(null);
    void startRun(goal);
  };

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    submitGoal();
  };

  return (
    <div className="app">
      <header className="command-bar">
        <div className="command-bar__main">
          <form onSubmit={handleSubmit}>
            <input
              type="text"
              value={goalInput}
              onChange={(e) => setGoalInput(e.target.value)}
              placeholder="요청을 입력하세요 (예: 간단한 랜딩 페이지)"
              aria-label="요청"
            />
            <Button variant="primary" loading={isRunning} onClick={submitGoal}>
              {isRunning ? "실행 중…" : "시작"}
            </Button>
          </form>
          {error && <p className="command-bar__error">{error}</p>}
        </div>
        <SearchBox />
      </header>
      <nav className="view-tabs" aria-label="뷰 전환">
        {TABS.map((tab) => (
          <button
            key={tab.key}
            type="button"
            aria-pressed={view === tab.key}
            className={view === tab.key ? "view-tabs__button view-tabs__button--active" : "view-tabs__button"}
            onClick={() => setView(tab.key)}
          >
            {tab.label}
          </button>
        ))}
      </nav>
      <main className="layout">
        <Rail />
        {view === "board" && <Board />}
        {view === "dag" && <DagView />}
        {view === "timeline" && <TimelineView />}
        {view === "inbox" && <Inbox />}
        {view === "artifacts" && <Artifacts />}
        <div className="layout__right">
          <Thread />
          <RosterPanel />
        </div>
      </main>
    </div>
  );
}
