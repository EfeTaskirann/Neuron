/* RunInspector — right-side panel for the Neuron prototype.
   Shows a run header, tabbed nav, an OTel-style span timeline, and a
   selected-span detail sheet with prompt/response for LLM spans.
   Loaded by Neuron App.html as <script type="text/babel"> after React,
   ReactDOM, babel-standalone, app/icons.jsx, and app/data.js. */
/* global React, NIcon */

const SPANS = [
  { id: "s0", name: "orchestrator.run",     indent: 0, type: "llm",   t0:    0, dur: 2400, running: false, attrs: { runId: "8e1c42a", agent: "Reasoner", model: "gpt-4o", temp: 0.4 } },
  { id: "s1", name: "llm.plan",             indent: 1, type: "llm",   t0:   40, dur:  680, running: false, attrs: { tokensIn: 412, tokensOut: 168, cost: 0.0024 }, prompt: "Plan the daily summary…", response: "1. Fetch overnight docs\n2. Search for blockers\n3. Synthesize and route." },
  { id: "s2", name: "tool.fetch_docs",      indent: 1, type: "tool",  t0:  720, dur:  340, running: false, attrs: { calls: 1, sourceCount: 12, cacheHit: false } },
  { id: "s3", name: "tool.search_web",      indent: 1, type: "tool",  t0:  720, dur:  520, running: false, attrs: { engine: "brave", results: 8 } },
  { id: "s4", name: "llm.synthesize",       indent: 1, type: "llm",   t0: 1240, dur:  920, running: true,  attrs: { tokensIn: 1820, tokensOut: 612, cost: 0.0072 }, prompt: "Given the docs and search results, draft today's summary.", response: "Today's standup highlights: 3 PRs merged, 1 blocker on the canvas refactor (animateMotion path memoization), and the on-call rotation hands off to mert at 17:00. " },
  { id: "s5", name: "logic.route",          indent: 2, type: "logic", t0: 2160, dur:  140, running: false, attrs: { branch: "needs-approval" } },
  { id: "s6", name: "human.approve",        indent: 1, type: "human", t0: 2300, dur:  100, running: false, attrs: { user: "Efe", channel: "slack:#daily" } },
];

const TOTAL = 2400; // ms — used for percentage math

/* ---------- helpers ---------- */
function formatDur(ms) {
  if (ms == null) return "—";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(2)}s`;
}

function formatAttrValue(v) {
  if (v === true) return "true";
  if (v === false) return "false";
  if (v == null) return "—";
  if (typeof v === "number") {
    if (Number.isInteger(v)) return v.toLocaleString();
    return v.toString();
  }
  return String(v);
}

/* ---------- SpanRow ---------- */
function SpanRow({ span, selected, onSelect }) {
  const left = (span.t0 / TOTAL) * 100;
  const width = (span.dur / TOTAL) * 100;
  const barClass = `span-bar kind-${span.type}${span.running ? " running wf-shimmer" : ""}`;
  const rowClass = `span-row${selected ? " selected" : ""}`;
  return (
    <div className={rowClass} onClick={() => onSelect(span.id)}>
      <div className="span-label" style={{ paddingLeft: 10 + span.indent * 16 }}>
        <span className={`span-dot kind-${span.type}`}/>
        <span className="span-name">{span.name}</span>
        <span className="span-glyph">{span.indent > 0 ? "└" : ""}</span>
      </div>
      <div className="span-track">
        <div
          className={barClass}
          style={{ left: `${left}%`, width: `${width}%` }}
        />
        <span className="span-dur">{formatDur(span.dur)}</span>
      </div>
    </div>
  );
}

/* ---------- SelectedSpanSheet ---------- */
function SelectedSpanSheet({ span }) {
  if (!span) return null;
  const attrs = span.attrs || {};
  const attrEntries = Object.entries(attrs);
  return (
    <div className="span-sheet">
      <div className="span-sheet-head">
        <span className={`span-dot kind-${span.type}`}/>
        <span className="span-sheet-name">{span.name}</span>
        <span className="span-sheet-meta">· {formatDur(span.dur)}</span>
        {span.running && (
          <span className="pill st-running"><span className="pulse-dot"/>running</span>
        )}
      </div>

      {attrEntries.length > 0 && (
        <div className="span-attrs">
          {attrEntries.map(([k, v]) => (
            <div key={k} className="span-attr-row">
              <span className="span-attr-k">{k}</span>
              <span className="span-attr-v">{formatAttrValue(v)}</span>
            </div>
          ))}
        </div>
      )}

      {span.type === "llm" && (span.prompt || span.response) && (
        <div className="span-llm">
          {span.prompt && (
            <div className="span-llm-block">
              <div className="span-llm-label">Prompt</div>
              <pre className="span-llm-snippet mute">{span.prompt}</pre>
            </div>
          )}
          {span.response && (
            <div className="span-llm-block">
              <div className="span-llm-label">Response</div>
              <pre className="span-llm-snippet">
                {span.response}
                {span.running && <span className="ins-stream-cursor"/>}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/* ---------- RunInspector ---------- */
function RunInspector({ onClose }) {
  const { useState } = React;
  const [selectedId, setSelectedId] = useState("s4");
  const [activeTab, setActiveTab] = useState("Spans");
  const selectedSpan = SPANS.find(s => s.id === selectedId) ?? SPANS[0];

  return (
    <div className="inspector">
      <div className="inspector-head">
        <div className="ins-head-l">
          <span className="ins-overline">Run · 8e1c42a</span>
          <h3 className="ins-title">Daily summary</h3>
        </div>
        <div className="ins-head-r">
          <span className="pill st-running"><span className="pulse-dot"/>Running · 0:42</span>
          <span className="pill st-outline">3,824 tokens</span>
          <span className="pill st-outline">$0.012</span>
          <button className="icon-btn" onClick={onClose} title="Close">
            <NIcon name="close" size={14}/>
          </button>
        </div>
      </div>

      <nav className="inspector-tabs">
        {["Spans", "Logs", "Output"].map(tab => (
          <button
            key={tab}
            className={`ins-tab${activeTab === tab ? " active" : ""}`}
            onClick={() => setActiveTab(tab)}
          >
            {tab}
          </button>
        ))}
      </nav>

      <div className="inspector-body">
        <div className="span-axis">
          <span>Span</span>
          <span className="span-axis-marks">
            <span>0ms</span>
            <span>1.2s</span>
            <span>2.4s</span>
          </span>
        </div>
        {SPANS.map(span => (
          <SpanRow
            key={span.id}
            span={span}
            selected={selectedId === span.id}
            onSelect={setSelectedId}
          />
        ))}
        <SelectedSpanSheet span={selectedSpan}/>
      </div>
    </div>
  );
}

window.RunInspector = RunInspector;
