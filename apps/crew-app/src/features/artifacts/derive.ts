import type { Envelope, SpecDoc, TaskDag } from "../../lib/types";
import { parseTaskResultBody } from "../thread/body";

type StoredMessage = { seq: number; envelope: Envelope };

export interface ArtifactVersion {
  msgSeq: number;
  ts: string;
  content: string;
}

export interface ArtifactVersions {
  taskId: string;
  name: string;
  versions: ArtifactVersion[];
}

/**
 * 계약 §E10: task.result 메시지들에서 (taskId, artifact.name)별 버전 리스트(seq 순).
 * taskId는 board/rail derive와 동일 관례로 envelope.corr(=task.assign과 공유하는
 * 상관관계 id, mock-source.ts 전 케이스에서 task.id와 동일)에서 얻는다. messages는
 * store가 seq 오름차순으로 append하므로 입력 순서를 그대로 신뢰(board/rail과 동일 관례).
 */
export function buildArtifactIndex(messages: StoredMessage[]): ArtifactVersions[] {
  const order: string[] = [];
  const byKey = new Map<string, ArtifactVersions>();

  for (const { seq, envelope } of messages) {
    if (envelope.kind !== "task.result") continue;
    const parsed = parseTaskResultBody(envelope.body);
    if (!parsed) continue;

    const taskId = envelope.corr;
    for (const artifact of parsed.artifacts) {
      const key = JSON.stringify([taskId, artifact.name]);
      let entry = byKey.get(key);
      if (!entry) {
        entry = { taskId, name: artifact.name, versions: [] };
        byKey.set(key, entry);
        order.push(key);
      }
      entry.versions.push({ msgSeq: seq, ts: envelope.ts, content: artifact.content });
    }
  }

  return order.map((key) => byKey.get(key)!);
}

export type DiffLineKind = "same" | "added" | "removed";

export interface DiffLine {
  kind: DiffLineKind;
  text: string;
}

/** DP 표가 매우 커지는 것(메모리 O(n·m))을 막는 방어선 — 초과 시 naive(전부 removed+added) 폴백. */
const LCS_LINE_LIMIT = 2000;

function naiveDiff(prevLines: string[], nextLines: string[]): DiffLine[] {
  return [
    ...prevLines.map((text): DiffLine => ({ kind: "removed", text })),
    ...nextLines.map((text): DiffLine => ({ kind: "added", text })),
  ];
}

/** 표준 LCS DP → 역추적. 동률(둘 다 LCS 기여 없음) 시 removed를 added보다 먼저 배출(관례 고정, D6 테스트 근거). */
function lcsDiff(prevLines: string[], nextLines: string[]): DiffLine[] {
  const n = prevLines.length;
  const m = nextLines.length;
  const lengths: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));

  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lengths[i][j] =
        prevLines[i] === nextLines[j] ? lengths[i + 1][j + 1] + 1 : Math.max(lengths[i + 1][j], lengths[i][j + 1]);
    }
  }

  const result: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (prevLines[i] === nextLines[j]) {
      result.push({ kind: "same", text: prevLines[i] });
      i++;
      j++;
    } else if (lengths[i + 1][j] >= lengths[i][j + 1]) {
      result.push({ kind: "removed", text: prevLines[i] });
      i++;
    } else {
      result.push({ kind: "added", text: nextLines[j] });
      j++;
    }
  }
  while (i < n) {
    result.push({ kind: "removed", text: prevLines[i] });
    i++;
  }
  while (j < m) {
    result.push({ kind: "added", text: nextLines[j] });
    j++;
  }
  return result;
}

/** 계약 §E10: 자체 라인 디프(LCS, 신규 의존 금지). prev=null(첫 버전) → 전체 added. */
export function lineDiff(prev: string | null, next: string): DiffLine[] {
  const nextLines = next.split("\n");
  if (prev === null) {
    return nextLines.map((text): DiffLine => ({ kind: "added", text }));
  }

  const prevLines = prev.split("\n");
  if (prevLines.length > LCS_LINE_LIMIT || nextLines.length > LCS_LINE_LIMIT) {
    return naiveDiff(prevLines, nextLines);
  }
  return lcsDiff(prevLines, nextLines);
}

export type ReqCellState = "expected" | "covered" | "none";

export interface ReqMatrixRow {
  reqId: string;
  cells: Record<string, ReqCellState>;
}

export interface ReqMatrix {
  taskIds: string[];
  rows: ReqMatrixRow[];
}

/**
 * 계약 §E10: 행=spec.requirements, 열=dag.tasks, 셀=covered(해당 task 최신 task.result.
 * covered_req_ids 포함) > expected(artifacts_expected.req_ids 포함) > none. 최신 판정은
 * messages 입력 순서(seq 오름차순 가정)에서 나중 값이 이전 값을 덮어쓰는 관례(board/rail derive와 동일).
 */
export function buildReqMatrix(spec: SpecDoc | null, dag: TaskDag | null, messages: StoredMessage[]): ReqMatrix {
  const tasks = dag?.tasks ?? [];
  const requirements = spec?.requirements ?? [];

  const latestCoveredByTask = new Map<string, Set<string>>();
  for (const { envelope } of messages) {
    if (envelope.kind !== "task.result") continue;
    const parsed = parseTaskResultBody(envelope.body);
    if (!parsed) continue;
    latestCoveredByTask.set(envelope.corr, new Set(parsed.covered_req_ids));
  }

  const rows: ReqMatrixRow[] = requirements.map((req) => {
    const cells: Record<string, ReqCellState> = {};
    for (const task of tasks) {
      const covered = latestCoveredByTask.get(task.id)?.has(req.id) ?? false;
      const expected = task.artifacts_expected.some((a) => a.req_ids.includes(req.id));
      cells[task.id] = covered ? "covered" : expected ? "expected" : "none";
    }
    return { reqId: req.id, cells };
  });

  return { taskIds: tasks.map((t) => t.id), rows };
}
