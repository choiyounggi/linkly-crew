import type { TaskDag, TaskStateDto } from "../../lib/types";

/** 계약 C7c verbatim (M5) — features/dag 내 로컬 복제(다른 feature import 금지). */
export const AVATAR_INITIALS: Record<string, string> = {
  lead: "LD",
  pm: "PM",
  designer: "DS",
  publisher: "PB",
  developer: "DV",
  qa: "QA",
};

export function avatarInitials(role: string): string {
  return AVATAR_INITIALS[role] ?? role.slice(0, 2).toUpperCase();
}

/** 계약 §D4 verbatim. */
export interface DagViewNode {
  id: string;
  role: string;
  state: TaskStateDto | "pending";
  layer: number;
  row: number;
  onCriticalPath: boolean;
}

/** 계약 §D4 verbatim. */
export interface DagViewEdge {
  from: string;
  to: string;
  onCriticalPath: boolean;
}

/** 전체 배열 요소별 localeCompare — 길이가 같을 때만 비교되므로 길이 분기는 안전망. */
function lexLess(a: string[], b: string[]): boolean {
  const len = Math.min(a.length, b.length);
  for (let i = 0; i < len; i++) {
    const cmp = a[i].localeCompare(b[i]);
    if (cmp !== 0) return cmp < 0;
  }
  return a.length < b.length;
}

/**
 * 계약 §D4 verbatim. layer=루트 최장 경로 깊이, row=layer 내 id 사전순 인덱스,
 * criticalPath=노드 수 기준 최장 체인(동률은 경로 배열 사전식 비교로 결정론).
 * 사이클 노드는 layer 0 + 크리티컬 패스 후보 제외(무한루프 금지).
 */
export function buildDagView(
  dag: TaskDag | null,
  taskStates: Record<string, TaskStateDto>,
): { nodes: DagViewNode[]; edges: DagViewEdge[]; criticalPath: string[] } {
  if (!dag || dag.tasks.length === 0) return { nodes: [], edges: [], criticalPath: [] };

  const tasks = dag.tasks;
  const byId = new Map(tasks.map((t) => [t.id, t]));

  const layer = new Map<string, number>();
  const inCycle = new Set<string>();
  const stack: string[] = [];

  function computeLayer(id: string): number {
    const cached = layer.get(id);
    if (cached !== undefined) return cached;
    const idx = stack.indexOf(id);
    if (idx !== -1) {
      for (let i = idx; i < stack.length; i++) inCycle.add(stack[i]);
      return 0;
    }
    stack.push(id);
    const deps = byId.get(id)?.deps ?? [];
    let maxDepLayer = -1;
    for (const dep of deps) {
      if (!byId.has(dep)) continue;
      const depLayer = computeLayer(dep);
      if (depLayer > maxDepLayer) maxDepLayer = depLayer;
    }
    stack.pop();
    const result = inCycle.has(id) ? 0 : maxDepLayer + 1;
    layer.set(id, result);
    return result;
  }

  for (const t of tasks) computeLayer(t.id);

  const byLayer = new Map<number, string[]>();
  for (const t of tasks) {
    const l = layer.get(t.id)!;
    if (!byLayer.has(l)) byLayer.set(l, []);
    byLayer.get(l)!.push(t.id);
  }
  const row = new Map<string, number>();
  for (const ids of byLayer.values()) {
    const sorted = [...ids].sort((a, b) => a.localeCompare(b));
    sorted.forEach((id, i) => row.set(id, i));
  }

  // criticalPath DP: 위상 순서(layer 오름차순, 동률 id 사전순)로 처리 — 사이클 진입/이탈 엣지는 후보에서 제외.
  const order = [...tasks].sort((a, b) => {
    const la = layer.get(a.id)!;
    const lb = layer.get(b.id)!;
    if (la !== lb) return la - lb;
    return a.id.localeCompare(b.id);
  });

  const chain = new Map<string, string[]>();
  for (const t of order) {
    if (inCycle.has(t.id)) {
      chain.set(t.id, [t.id]);
      continue;
    }
    let best: string[] | null = null;
    for (const dep of t.deps) {
      if (!byId.has(dep) || inCycle.has(dep)) continue;
      const depChain = chain.get(dep)!;
      if (best === null || depChain.length > best.length || (depChain.length === best.length && lexLess(depChain, best))) {
        best = depChain;
      }
    }
    chain.set(t.id, best ? [...best, t.id] : [t.id]);
  }

  let criticalPath: string[] = [];
  for (const t of tasks) {
    const c = chain.get(t.id)!;
    if (c.length > criticalPath.length || (c.length === criticalPath.length && lexLess(c, criticalPath))) {
      criticalPath = c;
    }
  }

  const criticalEdges = new Set<string>();
  for (let i = 0; i < criticalPath.length - 1; i++) {
    criticalEdges.add(`${criticalPath[i]}->${criticalPath[i + 1]}`);
  }
  const criticalNodes = new Set(criticalPath);

  const nodes: DagViewNode[] = tasks.map((t) => ({
    id: t.id,
    role: t.role,
    state: taskStates[t.id] ?? "pending",
    layer: layer.get(t.id)!,
    row: row.get(t.id)!,
    onCriticalPath: criticalNodes.has(t.id),
  }));

  const edges: DagViewEdge[] = [];
  for (const t of tasks) {
    for (const dep of t.deps) {
      if (!byId.has(dep)) continue;
      edges.push({ from: dep, to: t.id, onCriticalPath: criticalEdges.has(`${dep}->${t.id}`) });
    }
  }

  return { nodes, edges, criticalPath };
}
