/* WorkflowCanvas — main view of the `canvas` route.
   Renders absolute-positioned NodeCards over an SVG layer of bezier edges,
   with a dot-grid background, a minimap, and a vertical control strip.
   Loaded as <script type="text/babel"> after icons.jsx and data.js attached
   NIcon, NodeGlyph, Brandmark, NeuronData to window. */
/* global React */

const NODES = [
  { id: "n1", kind: "llm",   x:  60, y:  80, title: "Planner",     meta: "gpt-4o · 1.2k tok",       status: "success" },
  { id: "n2", kind: "tool",  x: 360, y:  40, title: "fetch_docs",  meta: "tool · 0.34s",            status: "success" },
  { id: "n3", kind: "tool",  x: 360, y: 200, title: "search_web",  meta: "tool · 0.52s",            status: "success" },
  { id: "n4", kind: "llm",   x: 660, y: 110, title: "Reasoner",    meta: "gpt-4o · 2.4k tok",       status: "running" },
  { id: "n5", kind: "human", x: 960, y:  70, title: "Approve",     meta: "human · waiting",         status: "waiting" },
  { id: "n6", kind: "logic", x: 960, y: 220, title: "Route",       meta: "logic · idle",            status: "idle"    },
];

const EDGES = [
  { from: "n1", to: "n2", active: false },
  { from: "n1", to: "n3", active: false },
  { from: "n2", to: "n4", active: true  },
  { from: "n3", to: "n4", active: true  },
  { from: "n4", to: "n5", active: false },
  { from: "n4", to: "n6", active: false },
];

const NODE_W = 220;
const NODE_H = 92;

/* ---------- NodeCard ---------- */
function NodeCard({ node, selected, onSelect }) {
  const cls = [
    "node-card",
    `status-${node.status}`,
    `kind-${node.kind}`,
    selected ? "selected" : "",
  ].filter(Boolean).join(" ");

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
        <NodeGlyph kind={node.kind} size={22} />
        <div className="node-card-title">{node.title}</div>
        <span className={`node-dot status-${node.status}`} aria-hidden="true" />
      </div>
      <div className="node-card-meta">{node.meta}</div>
    </div>
  );
}

/* ---------- Edge ---------- */
function Edge({ from, to, active }) {
  const fx = from.x + NODE_W;
  const fy = from.y + NODE_H / 2;
  const tx = to.x;
  const ty = to.y + NODE_H / 2;
  const cx = (fx + tx) / 2;
  const d  = `M ${fx} ${fy} C ${cx} ${fy}, ${cx} ${ty}, ${tx} ${ty}`;

  return (
    <path
      className={`canvas-edge${active ? " active" : ""}`}
      d={d}
      fill="none"
      stroke={active ? "var(--neuron-violet-500)" : "var(--border)"}
      strokeWidth={active ? 2 : 1.5}
      strokeDasharray={active ? "6 6" : "0"}
      strokeLinecap="round"
      style={active ? { animation: "edgeFlow 600ms linear infinite" } : undefined}
    />
  );
}

/* ---------- Minimap ---------- */
function Minimap({ nodes, edges, byId, bounds, selectedId }) {
  const pad = 40;
  const minX = Math.min(...nodes.map(n => n.x)) - pad;
  const minY = Math.min(...nodes.map(n => n.y)) - pad;
  const maxX = Math.max(...nodes.map(n => n.x + NODE_W)) + pad;
  const maxY = Math.max(...nodes.map(n => n.y + NODE_H)) + pad;
  const w = maxX - minX;
  const h = maxY - minY;

  const kindStroke = {
    llm:   "var(--neuron-violet-400)",
    tool:  "var(--neuron-sky-400)",
    logic: "var(--neuron-slate-400)",
    human: "var(--neuron-amber-400)",
    mcp:   "var(--neuron-violet-300)",
  };

  return (
    <div className="minimap" aria-hidden="true">
      <svg
        width="100%"
        height="100%"
        viewBox={`${minX} ${minY} ${w} ${h}`}
        preserveAspectRatio="xMidYMid meet"
      >
        {edges.map((e, i) => {
          const f = byId[e.from];
          const t = byId[e.to];
          return (
            <line
              key={i}
              x1={f.x + NODE_W / 2}
              y1={f.y + NODE_H / 2}
              x2={t.x + NODE_W / 2}
              y2={t.y + NODE_H / 2}
              stroke="var(--border)"
              strokeWidth="3"
            />
          );
        })}
        {nodes.map(n => (
          <rect
            key={n.id}
            x={n.x}
            y={n.y}
            width={NODE_W}
            height={NODE_H}
            rx="14"
            fill="var(--card)"
            stroke={selectedId === n.id ? "var(--neuron-violet-500)" : (kindStroke[n.kind] || "var(--border)")}
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

/* ---------- CanvasControls ---------- */
function CanvasControls() {
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

/* ---------- WorkflowCanvas ---------- */
function WorkflowCanvas({ onSelectRun }) {
  const { useState, useMemo } = React;
  const [selected, setSelected] = useState("n4");
  const byId = useMemo(() => Object.fromEntries(NODES.map(n => [n.id, n])), []);

  const handleSelect = (id) => {
    setSelected(id);
    if (typeof onSelectRun === "function") onSelectRun(id);
  };

  const right  = Math.max(...NODES.map(n => n.x + NODE_W)) + 80;
  const bottom = Math.max(...NODES.map(n => n.y + NODE_H)) + 80;

  return (
    <div className="canvas">
      <div className="canvas-grid" aria-hidden="true" />
      <svg
        className="canvas-edges"
        width={right}
        height={bottom}
        viewBox={`0 0 ${right} ${bottom}`}
      >
        {EDGES.map((e, i) => (
          <Edge key={i} from={byId[e.from]} to={byId[e.to]} active={e.active} />
        ))}
      </svg>
      <div className="canvas-nodes" style={{ width: right, height: bottom }}>
        {NODES.map(n => (
          <NodeCard
            key={n.id}
            node={n}
            selected={selected === n.id}
            onSelect={handleSelect}
          />
        ))}
      </div>
      <Minimap
        nodes={NODES}
        edges={EDGES}
        byId={byId}
        bounds={{ right, bottom }}
        selectedId={selected}
      />
      <CanvasControls />
    </div>
  );
}

window.WorkflowCanvas = WorkflowCanvas;
