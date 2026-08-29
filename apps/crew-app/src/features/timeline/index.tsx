import { useMemo } from "react";

import { useRunStore } from "../../lib/store";
import { buildTimeline } from "./derive";
import "./timeline.css";

function chipClass(kind: string): string {
  if (kind === "change_request") return "timeline-chip timeline-chip--change-request";
  if (kind === "human.gate") return "timeline-chip timeline-chip--gate";
  if (kind === "task.result") return "timeline-chip timeline-chip--result";
  if (kind === "task.ack") return "timeline-chip timeline-chip--ack";
  return "timeline-chip";
}

export default function TimelineView() {
  const messages = useRunStore((s) => s.messages);
  const sprintWindows = useRunStore((s) => s.sprintWindows);
  const { lanes, markers } = useMemo(
    () => buildTimeline(messages, sprintWindows),
    [messages, sprintWindows],
  );

  return (
    <section className="panel panel--timeline timeline-view" aria-label="타임라인">
      <h2>타임라인</h2>
      {lanes.length === 0 ? (
        <p>아직 활동이 없습니다</p>
      ) : (
        <div className="timeline-lanes">
          <div className="timeline-lanes__markers" aria-hidden="true">
            {markers.map((marker) => (
              <div key={marker.index} className="timeline-marker" style={{ left: `${marker.frac * 100}%` }}>
                <span className="timeline-marker__label">{`S${marker.index}`}</span>
              </div>
            ))}
          </div>
          {lanes.map((lane) => (
            <div key={lane.agentId} className="timeline-lane">
              <span className="timeline-lane__label">{lane.agentId}</span>
              <div className="timeline-lane__track">
                {lane.items.map((item, index) => (
                  <span
                    key={`${lane.agentId}-${index}`}
                    className={chipClass(item.kind)}
                    style={{ left: `${item.frac * 100}%` }}
                    title={`${item.ts} ${item.label}`}
                  />
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
