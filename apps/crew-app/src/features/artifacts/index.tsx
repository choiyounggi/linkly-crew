import { useMemo, useState } from "react";

import { useRunStore } from "../../lib/store";
import { buildArtifactIndex, buildReqMatrix, lineDiff, type ArtifactVersions } from "./derive";
import "./artifacts.css";

function artifactKey(a: { taskId: string; name: string }): string {
  return JSON.stringify([a.taskId, a.name]);
}

function ReqMatrix({ matrix }: { matrix: ReturnType<typeof buildReqMatrix> }) {
  if (matrix.rows.length === 0) {
    return <p className="artifacts-view__empty">REQ 정보 없음</p>;
  }

  return (
    <table className="req-matrix">
      <thead>
        <tr>
          <th scope="col">REQ</th>
          {matrix.taskIds.map((taskId) => (
            <th key={taskId} scope="col">
              {taskId}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {matrix.rows.map((row) => (
          <tr key={row.reqId}>
            <th scope="row">{row.reqId}</th>
            {matrix.taskIds.map((taskId) => {
              const state = row.cells[taskId];
              return (
                <td key={taskId} className={`req-matrix__cell req-matrix__cell--${state}`} aria-label={`${row.reqId} ${taskId} ${state}`} />
              );
            })}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export default function Artifacts() {
  const spec = useRunStore((s) => s.spec);
  const dag = useRunStore((s) => s.dag);
  const messages = useRunStore((s) => s.messages);

  const matrix = useMemo(() => buildReqMatrix(spec, dag, messages), [spec, dag, messages]);
  const artifactIndex = useMemo(() => buildArtifactIndex(messages), [messages]);

  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [selectedSeq, setSelectedSeq] = useState<number | null>(null);

  const selected: ArtifactVersions | null =
    artifactIndex.find((a) => artifactKey(a) === selectedKey) ?? artifactIndex[0] ?? null;

  const versions = selected?.versions ?? [];
  const latestSeq = versions.length > 0 ? versions[versions.length - 1].msgSeq : null;
  const activeSeq = selectedSeq ?? latestSeq;
  const activeIndex = versions.findIndex((v) => v.msgSeq === activeSeq);
  const activeVersion = activeIndex >= 0 ? versions[activeIndex] : null;
  const prevVersion = activeIndex > 0 ? versions[activeIndex - 1] : null;
  const diff = activeVersion ? lineDiff(prevVersion ? prevVersion.content : null, activeVersion.content) : [];

  return (
    <section className="artifacts-view" aria-label="아티팩트">
      <h2 className="panel__title">아티팩트</h2>
      <div className="artifacts-view__matrix">
        <ReqMatrix matrix={matrix} />
      </div>

      <div className="artifacts-view__body">
        {artifactIndex.length === 0 ? (
          <p className="artifacts-view__empty">아티팩트 준비 중</p>
        ) : (
          <>
            <ul className="artifact-list">
              {artifactIndex.map((a) => {
                const key = artifactKey(a);
                const isActive = selected !== null && artifactKey(selected) === key;
                return (
                  <li key={key}>
                    <button
                      type="button"
                      className={isActive ? "artifact-list__item artifact-list__item--active" : "artifact-list__item"}
                      onClick={() => {
                        setSelectedKey(key);
                        setSelectedSeq(null);
                      }}
                    >
                      <span className="artifact-list__task">{a.taskId}</span>
                      <span className="artifact-list__name">{a.name}</span>
                    </button>
                  </li>
                );
              })}
            </ul>

            <div className="artifact-detail">
              {selected && activeVersion ? (
                <>
                  <div className="artifact-detail__header">
                    <span className="artifact-detail__name">
                      {selected.taskId} / {selected.name}
                    </span>
                    <select
                      aria-label="버전 선택"
                      value={activeVersion.msgSeq}
                      onChange={(e) => setSelectedSeq(Number(e.target.value))}
                    >
                      {versions.map((v, i) => (
                        <option key={v.msgSeq} value={v.msgSeq}>
                          v{i + 1} (seq {v.msgSeq})
                        </option>
                      ))}
                    </select>
                  </div>
                  <pre className="artifact-detail__content">
                    {diff.map((line, i) => (
                      <div key={i} className={`diff-line diff-line--${line.kind}`}>
                        {line.text}
                      </div>
                    ))}
                  </pre>
                </>
              ) : (
                <p className="artifacts-view__empty">아티팩트를 선택하세요</p>
              )}
            </div>
          </>
        )}
      </div>
    </section>
  );
}
