import { Handle, Position, ReactFlow, type Edge, type Node, type NodeProps } from "@xyflow/react";
import { useMemo } from "react";

import { useRunStore } from "../../lib/store";
import type { TaskStateDto } from "../../lib/types";
import { avatarInitials, buildDagView, type DagViewNode } from "./derive";
import "@xyflow/react/dist/style.css";
import "./dag.css";

type NodeData = {
  taskId: string;
  initials: string;
  state: TaskStateDto | "pending";
  onCriticalPath: boolean;
};

function DagNode({ data }: NodeProps<Node<NodeData>>) {
  const showStateLabel = data.state === "escalated" || data.state === "blocked";
  return (
    <div className={`dag-node dag-node--${data.state}${data.onCriticalPath ? " dag-node--critical" : ""}`}>
      <Handle type="target" position={Position.Left} />
      <span className="dag-node__initials">{data.initials}</span>
      <span className="dag-node__id">{data.taskId}</span>
      {showStateLabel && <span className="dag-node__state">{data.state}</span>}
      <Handle type="source" position={Position.Right} />
    </div>
  );
}

const NODE_TYPES = { dagNode: DagNode };

function toFlowNode(n: DagViewNode): Node<NodeData> {
  return {
    id: n.id,
    type: "dagNode",
    position: { x: n.layer * 240, y: n.row * 96 },
    data: { taskId: n.id, initials: avatarInitials(n.role), state: n.state, onCriticalPath: n.onCriticalPath },
  };
}

export default function DagView() {
  const dag = useRunStore((s) => s.dag);
  const taskStates = useRunStore((s) => s.taskStates);

  const { nodes, edges } = useMemo(() => {
    const view = buildDagView(dag, taskStates);
    const flowNodes = view.nodes.map(toFlowNode);
    const flowEdges: Edge[] = view.edges.map((e) => ({
      id: `${e.from}-${e.to}`,
      source: e.from,
      target: e.to,
      style: e.onCriticalPath ? { stroke: "var(--accent)", strokeWidth: 3 } : undefined,
    }));
    return { nodes: flowNodes, edges: flowEdges };
  }, [dag, taskStates]);

  if (nodes.length === 0) {
    return (
      <section className="dag-view dag-view--empty" aria-label="DAG 뷰">
        아직 DAG가 없습니다
      </section>
    );
  }

  return (
    <section className="dag-view" aria-label="DAG 뷰">
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={NODE_TYPES}
        fitView
        nodesDraggable={false}
        nodesConnectable={false}
      />
    </section>
  );
}
