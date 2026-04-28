/* global React, NeuronUI */
const { useState, useMemo } = React;
const { Icon, StatusDot } = NeuronUI;

// Mock fixture
const NODES = [
  { id: "n1", type: "llm",   x:  60, y: 80,  title: "Planner",     meta: "gpt-4o · 1.2k tok", status: "success" },
  { id: "n2", type: "tool",  x: 360, y: 40,  title: "fetch_docs",  meta: "tool · ready",      status: "online" },
  { id: "n3", type: "tool",  x: 360, y: 180, title: "search_web",  meta: "tool · ready",      status: "online" },
  { id: "n4", type: "llm",   x: 660, y: 110, title: "Reasoner",    meta: "gpt-4o · 2.4k tok", status: "running" },
  { id: "n5", type: "human", x: 960, y: 70,  title: "Approve",     meta: "human · waiting",   status: "degraded" },
  { id: "n6", type: "logic", x: 960, y: 200, title: "Route",       meta: "logic · idle",      status: "offline" },
];
const EDGES = [
  { from: "n1", to: "n2", active: false },
  { from: "n1", to: "n3", active: false },
  { from: "n2", to: "n4", active: true  },
  { from: "n3", to: "n4", active: true  },
  { from: "n4", to: "n5", active: false },
  { from: "n4", to: "n6", active: false },
];

const nodeStyles = {
  llm:   { color: "var(--neuron-violet-400)", stripe: "var(--neuron-violet-500)", icon: "sparkles" },
  tool:  { color: "var(--neuron-sky-500)",    stripe: "var(--neuron-sky-500)",    icon: "wrench" },
  logic: { color: "var(--neuron-slate-500)",  stripe: "var(--neuron-slate-500)",  icon: "branch" },
  human: { color: "var(--neuron-amber-500)",  stripe: "var(--neuron-amber-500)",  icon: "hand" },
};

const NodeCard = ({ node, selected, onSelect }) => {
  const s = nodeStyles[node.type];
  const [hover, setHover] = useState(false);
  const running = node.status === "running";
  const lift = hover && !selected ? "translateY(-1px)" : "translateY(0)";
  const shadow = selected ? "var(--glow-violet-md)"
    : running ? "var(--glow-violet-sm)"
    : hover ? "var(--shadow-md)" : "var(--shadow-sm)";
  return (
    <div
      onClick={() => onSelect(node.id)}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        position: "absolute", left: node.x, top: node.y, width: 220,
        background: "var(--card)", borderRadius: 16,
        border: selected ? "1px solid transparent" : "1px solid var(--border)",
        padding: "12px 14px 12px 16px", cursor: "pointer",
        transition: "all 160ms var(--ease-out)", transform: lift, boxShadow: shadow,
        animation: running ? "nodePulse 1.6s ease-in-out infinite" : "none",
      }}
    >
      <div style={{ position: "absolute", left: 0, top: 10, bottom: 10, width: 3, borderRadius: 2, background: s.stripe }} />
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
        <Icon name={s.icon} size={18} color={s.color} />
        <div style={{ fontSize: 14, fontWeight: 600, flex: 1, color: "var(--foreground)" }}>{node.title}</div>
        <StatusDot variant={node.status} />
      </div>
      <div style={{ fontSize: 11, color: "var(--muted-foreground)", fontFamily: "var(--font-mono)" }}>{node.meta}</div>
    </div>
  );
};

const Edge = ({ from, to, active }) => {
  // bezier path between right side of from node to left side of to node
  const fx = from.x + 220, fy = from.y + 40;
  const tx = to.x,         ty = to.y + 40;
  const cx = (fx + tx) / 2;
  const d = `M ${fx} ${fy} C ${cx} ${fy}, ${cx} ${ty}, ${tx} ${ty}`;
  return (
    <path d={d} fill="none"
      stroke={active ? "var(--neuron-violet-500)" : "var(--border)"}
      strokeWidth={active ? 2 : 1.5}
      strokeDasharray={active ? "6 4" : "0"}
      style={active ? { animation: "edgeFlow 600ms linear infinite" } : {}}
    />
  );
};

const Canvas = ({ selectedId, onSelect }) => {
  const byId = useMemo(() => Object.fromEntries(NODES.map(n => [n.id, n])), []);
  return (
    <div style={{
      position: "relative", width: "100%", height: "100%",
      background: "var(--background)",
      backgroundImage: "radial-gradient(oklch(0.265 0.068 258) 1px, transparent 1px)",
      backgroundSize: "24px 24px",
      overflow: "hidden",
    }}>
      <svg style={{ position: "absolute", inset: 0, width: "100%", height: "100%", pointerEvents: "none" }}>
        {EDGES.map((e, i) => (
          <Edge key={i} from={byId[e.from]} to={byId[e.to]} active={e.active} />
        ))}
      </svg>
      {NODES.map(n => (
        <NodeCard key={n.id} node={n} selected={selectedId === n.id} onSelect={onSelect} />
      ))}
      {/* Minimap */}
      <div style={{
        position: "absolute", left: 16, bottom: 16, width: 160, height: 100,
        background: "oklch(0.190 0.046 258 / 0.7)", backdropFilter: "blur(10px)",
        borderRadius: 12, border: "1px solid var(--border)", padding: 8,
      }}>
        <svg width="100%" height="100%" viewBox="0 0 1200 320" preserveAspectRatio="xMidYMid meet">
          {EDGES.map((e, i) => {
            const f = byId[e.from], t = byId[e.to];
            return <line key={i} x1={f.x+110} y1={f.y+40} x2={t.x+110} y2={t.y+40} stroke="var(--neuron-midnight-600)" strokeWidth="3" />;
          })}
          {NODES.map(n => <rect key={n.id} x={n.x} y={n.y} width={220} height={80} rx={12} fill="var(--neuron-midnight-700)" stroke={nodeStyles[n.type].stripe} strokeWidth="2" />)}
          <rect x={20} y={20} width={1160} height={280} fill="none" stroke="var(--neuron-violet-400)" strokeWidth="6" rx={20} />
        </svg>
      </div>
      {/* Controls */}
      <div style={{
        position: "absolute", right: 16, bottom: 16, display: "flex", flexDirection: "column",
        background: "var(--card)", borderRadius: 8, border: "1px solid var(--border)",
        boxShadow: "var(--shadow-sm)", overflow: "hidden",
      }}>
        {["plus", "search", "settings"].map((n, i) => (
          <button key={n} style={{
            width: 32, height: 32, background: "transparent", border: "none",
            borderTop: i ? "1px solid var(--border)" : "none",
            color: "var(--muted-foreground)", cursor: "pointer",
            display: "grid", placeItems: "center",
          }}>
            <Icon name={n} size={14} />
          </button>
        ))}
      </div>
    </div>
  );
};

window.NeuronCanvas = Canvas;
