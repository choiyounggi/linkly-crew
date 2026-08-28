import { useEffect, useState, type FormEvent } from "react";

import Board from "./features/board";
import Rail from "./features/rail";
import RosterPanel from "./features/roster";
import Thread from "./features/thread";
import { defaultSource, useRunStore } from "./lib/store";
import "./App.css";

export default function App() {
  const [goalInput, setGoalInput] = useState("");
  const [error, setError] = useState<string | null>(null);
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

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    const goal = goalInput.trim();
    if (!goal) {
      setError("요청을 입력하세요");
      return;
    }
    setError(null);
    void startRun(goal);
  };

  return (
    <div className="app">
      <header className="command-bar">
        <form onSubmit={handleSubmit}>
          <input
            type="text"
            value={goalInput}
            onChange={(e) => setGoalInput(e.target.value)}
            placeholder="요청을 입력하세요 (예: 간단한 랜딩 페이지)"
            aria-label="요청"
          />
          <button type="submit" disabled={isRunning}>
            {isRunning ? "실행 중…" : "시작"}
          </button>
        </form>
        {error && <p className="command-bar__error">{error}</p>}
      </header>
      <main className="layout">
        <Rail />
        <Board />
        <div className="layout__right">
          <Thread />
          <RosterPanel />
        </div>
      </main>
    </div>
  );
}
