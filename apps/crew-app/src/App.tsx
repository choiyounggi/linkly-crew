import { useEffect, useState } from "react";

import ChatPane from "./features/chat";
import NewTaskModal from "./features/channels/NewTaskModal";
import OnboardingFlow from "./features/onboarding";
import Sidebar from "./features/channels/Sidebar";
import SettingsMenu from "./features/settings";
import { Button } from "./components/primitives";
import { defaultSource, useRunStore } from "./lib/store";
import "./App.css";

/** plan D7: localStorage flag routing — t6 sets it in OnboardingFlow's onComplete. */
const ONBOARDED_KEY = "crew.onboarded";

export default function App() {
  const [onboarded, setOnboarded] = useState(() => localStorage.getItem(ONBOARDED_KEY) !== null);
  const [showSettings, setShowSettings] = useState(false);
  const [showNewTask, setShowNewTask] = useState(false);

  const applyEvent = useRunStore((s) => s.applyEvent);
  const activeRunId = useRunStore((s) => s.activeRunId);
  const activeChannel = useRunStore((s) => (s.activeRunId ? s.channels[s.activeRunId] : null));
  const closeChannel = useRunStore((s) => s.closeChannel);

  // One effect wires the subscription; applying the event is the store's job.
  useEffect(() => {
    return defaultSource.onEvent(applyEvent);
  }, [applyEvent]);

  if (!onboarded) {
    return (
      <OnboardingFlow
        onComplete={() => {
          localStorage.setItem(ONBOARDED_KEY, "1");
          setOnboarded(true);
        }}
      />
    );
  }

  return (
    <div className="app-shell">
      <Sidebar onNewTask={() => setShowNewTask(true)} onOpenSettings={() => setShowSettings(true)} />

      <div className="app-shell__main">
        {activeRunId && activeChannel ? (
          <>
            <header className="channel-header">
              <h1 className="channel-header__title">{activeChannel.goal || activeRunId}</h1>
              <Button variant="ghost" size="sm" onClick={() => void closeChannel(activeRunId)}>
                채널 닫기
              </Button>
            </header>
            <div className="channel-view">
              <ChatPane runId={activeRunId} />
              <aside id="channel-side-panel" className="channel-view__side" aria-label="상세 패널" />
            </div>
          </>
        ) : (
          <div className="app-shell__empty">
            <p>아직 채널이 없습니다</p>
            <Button variant="primary" onClick={() => setShowNewTask(true)}>
              새 작업 시작
            </Button>
          </div>
        )}
      </div>

      {showNewTask && <NewTaskModal onClose={() => setShowNewTask(false)} />}
      {showSettings && (
        <div className="modal-overlay">
          <SettingsMenu onClose={() => setShowSettings(false)} />
        </div>
      )}
    </div>
  );
}
