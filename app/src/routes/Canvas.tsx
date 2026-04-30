// Ports `Neuron Design/app/canvas.jsx::WorkflowCanvas`. Hardcoded
// NODES/EDGES → useWorkflow('daily-summary'). Field renames at the
// edge level — backend ships `fromNode`/`toNode`, the prototype's
// `from`/`to` keys go away (cleaner than an adapter).
//
// Inspector lives in `RunInspector.tsx` (phase C/2); this file
// covers the canvas itself.
import { useMemo, useState } from 'react';
import { NIcon, NodeGlyph, type NodeKind } from '../components/icons';
import { useWorkflow } from '../hooks/useWorkflow';
import type { Edge as EdgeRow, Node as NodeRow } from '../lib/bindings';

const NODE_W = 220;
const NODE_H = 92;

interface CanvasProps {
  workflowId?: string;
  onSelectNode?: (id: string) => void;
}

export function Canvas({ workflowId = 'daily-summary', onSelectNode }: CanvasProps): JSX.Element {
  const { data, isLoading, isError, error } = useWorkflow(workflowId);
  const [selected, setSelected] = useState<string | null>(null);

  const nodes = data?.nodes ?? [];
  const edges = data?.edges ?? [];
  const byId = useMemo(
    () => Object.fromEntries(nodes.map((n) => [n.id, n])),
    [nodes],
  );

  if (isLoading) {
    return <div className="canvas canvas-loading">Loading canvas…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  if (nodes.length === 0) {
    return (
      <div className="canvas canvas-empty">
        <p className="text-muted">No nodes yet for this workflow.</p>
      </div>
    );
  }

  const handleSelect = (id: string) => {
    setSelected(id);
    onSelectNode?.(id);
  };

  const right = Math.max(...nodes.map((n) => n.x + NODE_W)) + 80;
  const bottom = Math.max(...nodes.map((n) => n.y + NODE_H)) + 80;

  return (
    <div className="canvas">
      <div className="canvas-grid" aria-hidden="true" />
      <svg
        className="canvas-edges"
        width={right}
        height={bottom}
        viewBox={`0 0 ${right} ${bottom}`}
      >
        {edges.map((e) => {
          const from = byId[e.fromNode];
          const to = byId[e.toNode];
          if (!from || !to) return null;
          return <Edge key={e.id} from={from} to={to} active={e.active} />;
        })}
      </svg>
      <div className="canvas-nodes" style={{ width: right, height: bottom }}>
        {nodes.map((n) => (
          <NodeCard
            key={n.id}
            node={n}
            selected={selected === n.id}
            onSelect={handleSelect}
          />
        ))}
      </div>
      <Minimap nodes={nodes} edges={edges} byId={byId} selectedId={selected} />
      <CanvasControls />
    </div>
  );
}

interface NodeCardProps {
  node: NodeRow;
  selected: boolean;
  onSelect: (id: string) => void;
}

function NodeCard({ node, selected, onSelect }: NodeCardProps): JSX.Element {
  const cls = [
    'node-card',
    `status-${node.status}`,
    `kind-${node.kind}`,
    selected ? 'selected' : '',
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <div
      className={cls}
      style={{ left: node.x, top: node.y }}
      onClick={() => onSelect(node.id)}
      role="button"
      tabIndex={0}
    >
      <div className="node-rail" aria-hidden="true" />
      <div className="node-card-head">
        <NodeGlyph kind={node.kind as NodeKind} size={22} />
        <div className="node-card-title">{node.title}</div>
        <span className={`node-dot status-${node.status}`} aria-hidden="true" />
      </div>
      <div className="node-card-meta">{node.meta}</div>
    </div>
  );
}

interface EdgeProps {
  from: NodeRow;
  to: NodeRow;
  active: boolean;
}

function Edge({ from, to, active }: EdgeProps): JSX.Element {
  const fx = from.x + NODE_W;
  const fy = from.y + NODE_H / 2;
  const tx = to.x;
  const ty = to.y + NODE_H / 2;
  const cx = (fx + tx) / 2;
  const d = `M ${fx} ${fy} C ${cx} ${fy}, ${cx} ${ty}, ${tx} ${ty}`;
  return (
    <path
      className={`canvas-edge${active ? ' active' : ''}`}
      d={d}
      fill="none"
      stroke={active ? 'var(--neuron-violet-500)' : 'var(--border)'}
      strokeWidth={active ? 2 : 1.5}
      strokeDasharray={active ? '6 6' : '0'}
      strokeLinecap="round"
      style={active ? { animation: 'edgeFlow 600ms linear infinite' } : undefined}
    />
  );
}

interface MinimapProps {
  nodes: NodeRow[];
  edges: EdgeRow[];
  byId: Record<string, NodeRow>;
  selectedId: string | null;
}

function Minimap({ nodes, edges, byId, selectedId }: MinimapProps): JSX.Element {
  const pad = 40;
  const minX = Math.min(...nodes.map((n) => n.x)) - pad;
  const minY = Math.min(...nodes.map((n) => n.y)) - pad;
  const maxX = Math.max(...nodes.map((n) => n.x + NODE_W)) + pad;
  const maxY = Math.max(...nodes.map((n) => n.y + NODE_H)) + pad;
  const w = maxX - minX;
  const h = maxY - minY;

  const kindStroke: Record<string, string> = {
    llm: 'var(--neuron-violet-400)',
    tool: 'var(--neuron-sky-400)',
    logic: 'var(--neuron-slate-400)',
    human: 'var(--neuron-amber-400)',
    mcp: 'var(--neuron-violet-300)',
  };

  return (
    <div className="minimap" aria-hidden="true">
      <svg
        width="100%"
        height="100%"
        viewBox={`${minX} ${minY} ${w} ${h}`}
        preserveAspectRatio="xMidYMid meet"
      >
        {edges.map((e) => {
          const f = byId[e.fromNode];
          const t = byId[e.toNode];
          if (!f || !t) return null;
          return (
            <line
              key={e.id}
              x1={f.x + NODE_W / 2}
              y1={f.y + NODE_H / 2}
              x2={t.x + NODE_W / 2}
              y2={t.y + NODE_H / 2}
              stroke="var(--border)"
              strokeWidth="3"
            />
          );
        })}
        {nodes.map((n) => (
          <rect
            key={n.id}
            x={n.x}
            y={n.y}
            width={NODE_W}
            height={NODE_H}
            rx="14"
            fill="var(--card)"
            stroke={
              selectedId === n.id
                ? 'var(--neuron-violet-500)'
                : kindStroke[n.kind] ?? 'var(--border)'
            }
            strokeWidth={selectedId === n.id ? 6 : 3}
          />
        ))}
        <rect
          x={minX + 8}
          y={minY + 8}
          width={w - 16}
          height={h - 16}
          fill="none"
          stroke="var(--neuron-violet-400)"
          strokeWidth="4"
          rx="18"
          strokeDasharray="10 8"
        />
      </svg>
    </div>
  );
}

function CanvasControls(): JSX.Element {
  return (
    <div className="canvas-controls" role="toolbar" aria-label="Canvas controls">
      <button className="icon-btn" type="button" title="Zoom in" aria-label="Zoom in">
        <NIcon name="plus" size={14} />
      </button>
      <button className="icon-btn" type="button" title="Zoom out" aria-label="Zoom out">
        <svg
          className="n-icon n-icon-minus"
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.75"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M5 12h14" />
        </svg>
      </button>
      <button className="icon-btn" type="button" title="Fit to view" aria-label="Fit to view">
        <NIcon name="refresh" size={14} />
      </button>
      <button className="icon-btn" type="button" title="Lock canvas" aria-label="Lock canvas">
        <NIcon name="settings" size={14} />
      </button>
    </div>
  );
}
