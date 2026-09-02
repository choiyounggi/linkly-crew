// Slack-shaped left sidebar (plan D6): channel list (label + status badge),
// a "+" new-task button, and a settings button. Status is derived from
// `ChannelState.finished` at render time, never stored separately (plan D2).

import { Badge, IconButton } from "../../components/primitives";
import { useRunStore } from "../../lib/store";
import type { ChannelState } from "../../lib/store";
import "./sidebar.css";

interface SidebarProps {
  onNewTask: () => void;
  onOpenSettings: () => void;
}

type ChannelStatus = "running" | "completed" | "failed";

function channelStatus(channel: ChannelState): ChannelStatus {
  if (channel.finished === "completed") return "completed";
  if (channel.finished === "failed") return "failed";
  return "running";
}

const STATUS_LABEL: Record<ChannelStatus, string> = {
  running: "진행 중",
  completed: "완료",
  failed: "실패",
};

const STATUS_BADGE_VARIANT: Record<ChannelStatus, "accent" | "success" | "danger"> = {
  running: "accent",
  completed: "success",
  failed: "danger",
};

export default function Sidebar({ onNewTask, onOpenSettings }: SidebarProps) {
  const channelOrder = useRunStore((s) => s.channelOrder);
  const channels = useRunStore((s) => s.channels);
  const activeRunId = useRunStore((s) => s.activeRunId);
  const selectChannel = useRunStore((s) => s.selectChannel);
  const closeChannel = useRunStore((s) => s.closeChannel);

  return (
    <nav className="sidebar" aria-label="채널 목록">
      <div className="sidebar__header">
        <span className="sidebar__title">채널</span>
        <IconButton label="새 작업" onClick={onNewTask}>
          +
        </IconButton>
      </div>

      {channelOrder.length === 0 ? (
        <p className="sidebar__empty">진행 중인 채널이 없습니다</p>
      ) : (
        <ul className="sidebar__list">
          {channelOrder.map((runId) => {
            const channel = channels[runId];
            if (!channel) return null;
            const status = channelStatus(channel);
            return (
              <li key={runId} className="sidebar__item">
                <button
                  type="button"
                  className={
                    runId === activeRunId
                      ? "sidebar__channel sidebar__channel--active"
                      : "sidebar__channel"
                  }
                  aria-pressed={runId === activeRunId}
                  onClick={() => selectChannel(runId)}
                >
                  <span className="sidebar__channel-label">{channel.goal || runId}</span>
                  <Badge variant={STATUS_BADGE_VARIANT[status]}>{STATUS_LABEL[status]}</Badge>
                </button>
                <IconButton
                  label={`채널 닫기: ${channel.goal || runId}`}
                  onClick={() => void closeChannel(runId)}
                >
                  ×
                </IconButton>
              </li>
            );
          })}
        </ul>
      )}

      <div className="sidebar__footer">
        <IconButton label="설정" onClick={onOpenSettings}>
          ⚙
        </IconButton>
      </div>
    </nav>
  );
}
