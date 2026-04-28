/* global React, NeuronUI */
const { useState } = React;
const { Icon, Badge, StatusDot } = NeuronUI;

const SPANS = [
  { id: "s0", name: "orchestrator.run", indent: 0, type: "llm",   start: 0,   width: 100, dur: "2.4s" },
  { id: "s1", name: "llm.plan",         indent: 1, type: "llm",   start: 2,   width: 28,  dur: "0.68s" },
  { id: "s2", name: "tool.fetch_docs",  indent: 1, type: "tool",  start: 30,  width: 14,  dur: "0.34s" },
  { id: "s3", name: "tool.search_web",  indent: 1, type: "tool",  start: 30,  width: 22,  dur: "0.52s" },
  { id: "s4", name: "llm.synthesize",   indent: 1, type: "llm",   start: 52,  width: 38,  dur: "0.92s", running: true },
  { id: "s5", name: "logic.route",      indent: 2, type: "logic", start: 90,  width: 6,   dur: "0.14s" },
  { id: "s6", name: "human.approve",    indent: 1, type: "human", start: 96,  width: 4,   dur: "—" },
];

const typeColor = { llm: "var(--neuron-violet-500)", tool: "var(--neuron-sky-500)", logic: "var(--neuron-slate-500)", human: "var(--neuron-amber-500)" };

const SpanRow = ({ span, selected, onSelect }) => {
  const [hover, setHover] = useState(false);
  return (
    <div onClick={() => onSelect(span.id)} onMouseEnter={() => setHover(true)} onMouseLeave={() => setHover(false)}
      style={{ display: "grid", gridTemplateColumns: "260px 1fr", height: 28, cursor: "pointer",
        background: selected ? "var(--accent)" : hover ? "var(--muted)" : "transparent",
        borderLeft: selected ? "2px solid var(--neuron-violet-500)" : "2px solid transparent",
      }}>
      <div style={{ display: "flex", alignItems: "center", gap: 6, padding: `0 12px 0 ${10 + span.indent * 16}px`, fontSize: 12, color: "var(--foreground)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
        <span style={{ width: 6, height: 6, borderRadius: 9999, background: typeColor[span.type], flex: "none" }} />
        <span style={{ fontFamily: "var(--font-mono)" }}>{span.name}</span>
      </div>
      <div style={{ position: "relative", padding: "7px 12px" }}>
        <div style={{
          position: "absolute", top: 7, height: 14, borderRadius: 3,
          left: `${span.start}%`, width: `${span.width}%`,
          background: typeColor[span.type],
          boxShadow: span.running ? "var(--glow-violet-sm)" : "none",
          animation: span.running ? "spanPulse 1.6s ease-in-out infinite" : "none",
        }} />
        <span style={{ position: "absolute", top: 6, right: 12, fontFamily: "var(--font-mono)", fontSize: 10, color: "var(--muted-foreground)" }}>{span.dur}</span>
      </div>
    </div>
  );
};

const RunInspector = () => {
  const [selected, setSelected] = useState("s4");
  const span = SPANS.find(s => s.id === selected);
  return (
    <div style={{ display: "grid", gridTemplateRows: "auto 1fr", height: "100%", background: "var(--background)" }}>
      <div style={{ padding: "14px 20px", borderBottom: "1px solid var(--border)", display: "flex", alignItems: "center", justifyContent: "space-between" }}>
        <div>
          <div style={{ fontSize: 11, fontWeight: 600, letterSpacing: "0.08em", textTransform: "uppercase", color: "var(--muted-foreground)" }}>Run · 8e1c42a</div>
          <div style={{ fontSize: 18, fontWeight: 600, marginTop: 2 }}>Daily summary</div>
        </div>
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <Badge><StatusDot variant="running" />Running · 0:42</Badge>
          <Badge variant="outline">3,824 tokens</Badge>
          <Badge variant="outline">$0.012</Badge>
        </div>
      </div>
      <div style={{ overflow: "auto" }}>
        <div style={{ display: "grid", gridTemplateColumns: "260px 1fr", height: 24, borderBottom: "1px solid var(--border)", background: "var(--card)", position: "sticky", top: 0 }}>
          <div style={{ padding: "5px 12px", fontSize: 10, fontWeight: 600, letterSpacing: "0.06em", textTransform: "uppercase", color: "var(--muted-foreground)" }}>Span</div>
          <div style={{ padding: "5px 12px", fontSize: 10, fontWeight: 600, letterSpacing: "0.06em", textTransform: "uppercase", color: "var(--muted-foreground)", display: "flex", justifyContent: "space-between" }}>
            <span>0ms</span><span>1.2s</span><span>2.4s</span>
          </div>
        </div>
        {SPANS.map(s => <SpanRow key={s.id} span={s} selected={selected === s.id} onSelect={setSelected} />)}
      </div>
    </div>
  );
};

window.NeuronRunInspector = RunInspector;
