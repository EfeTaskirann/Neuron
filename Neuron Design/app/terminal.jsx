/* Terminal — pane grid, chrome, body, status bar.
   Mock-only: no real PTY. Renders a snapshot of the Daily summary scenario. */
/* global React */
const { useState, useMemo } = React;

function TerminalRoute() {
  const data = window.NeuronTerminalData;
  const [layout, setLayout] = useState("2x2");
  const [activeId, setActiveId] = useState("p1");
  const panes = data.panes;

  return (
    <div className="term-route">
      <div className={`pane-grid layout-${layout}`}>
        {panes.map(p => (
          <Pane key={p.id} pane={p} active={p.id === activeId}
                onActivate={() => setActiveId(p.id)}/>
        ))}
      </div>
      <TermStatusBar layout={layout} setLayout={setLayout} panes={panes}/>
    </div>
  );
}

function Pane({ pane, active, onActivate }) {
  const data = window.NeuronTerminalData;
  const agent = data.agents[pane.agent];
  return (
    <div className={`pane status-${pane.status}${active?" active":""}`}
         onClick={onActivate}>
      <div className="pane-stripe"/>
      <PaneHeader pane={pane} agent={agent}/>
      {pane.approval && <ApprovalBanner approval={pane.approval} agent={agent}/>}
      <PaneBody pane={pane}/>
    </div>
  );
}

function PaneHeader({ pane, agent }) {
  const statusLabel = {
    idle: "idle", running: "running", awaiting_approval: "awaiting",
    success: "done", error: "error", starting: "starting"
  }[pane.status] || pane.status;
  return (
    <div className="pane-head">
      <div className="pane-head-l">
        <AgentIcon kind={agent.icon} accent={agent.accent}/>
        <span className="pane-name">{agent.name}</span>
        <span className={`pane-dot status-${pane.status}`}/>
        <span className="pane-status">{statusLabel}</span>
        {pane.role && <span className="pane-role">· {pane.role}</span>}
      </div>
      <div className="pane-cwd" title={pane.cwd}>{pane.cwd}</div>
      <div className="pane-head-r">
        <button className="icon-btn sm" title="Clear"><NIcon name="trash" size={12}/></button>
        <button className="icon-btn sm" title="Restart"><NIcon name="play" size={12}/></button>
        <button className="icon-btn sm" title="Pop out"><NIcon name="layers" size={12}/></button>
        <button className="icon-btn sm" title="Close"><NIcon name="close" size={12}/></button>
      </div>
    </div>
  );
}

function ApprovalBanner({ approval, agent }) {
  return (
    <div className="approval-banner">
      <span className="ab-tag">tool</span>
      <code className="ab-tool">{approval.tool}</code>
      <span className="ab-arrow">→</span>
      <code className="ab-target">{approval.target}</code>
      <span className="ab-diff">
        <span className="ab-add">+{approval.added}</span>
        <span className="ab-rem">−{approval.removed}</span>
      </span>
      <div className="ab-spacer"/>
      <button className="btn ghost sm">Reject</button>
      <button className="btn primary sm">Accept</button>
    </div>
  );
}

function PaneBody({ pane }) {
  return (
    <div className="pane-body">
      {pane.lines.map((ln, i) => <TermLine key={i} line={ln}/>)}
      {pane.status === "running" && (
        <div className="term-cursor-line">
          <span className="term-cursor"/>
        </div>
      )}
    </div>
  );
}

function TermLine({ line }) {
  if (line.k === "prompt") {
    return (
      <div className="tl prompt">
        <span className="tl-prompt-sigil">{line.text || "›"}</span>
        {line.inline && <span className="tl-cmd"> {line.inline}</span>}
      </div>
    );
  }
  if (line.k === "command") {
    return <div className="tl command"><span className="tl-prompt-sigil">›</span> <span className="tl-cmd">{line.text}</span></div>;
  }
  if (line.k === "thinking") {
    return <div className="tl thinking"><span className="tl-think-dot"/> {line.text}</div>;
  }
  if (line.k === "tool") {
    return <div className="tl tool"><NIcon name="wrench" size={11}/> <span>{line.text}</span></div>;
  }
  if (line.k === "err")  return <div className="tl err">{line.text}</div>;
  if (line.k === "sys")  return <div className="tl sys">{line.text}</div>;
  return <div className="tl out">{line.text}</div>;
}

function TermStatusBar({ layout, setLayout, panes }) {
  const counts = useMemo(()=>{
    const c = { running:0, idle:0, error:0, awaiting:0, success:0 };
    for (const p of panes) {
      if (p.status === "running") c.running++;
      else if (p.status === "idle") c.idle++;
      else if (p.status === "error") c.error++;
      else if (p.status === "awaiting_approval") c.awaiting++;
      else if (p.status === "success") c.success++;
    }
    return c;
  }, [panes]);

  return (
    <div className="term-statusbar">
      <div className="tsb-l">
        <div className="tsb-ws"><NIcon name="layers" size={12}/> personal · {panes.length} panes</div>
      </div>
      <div className="tsb-c">
        {counts.running   ? <span className="tsb-pill st-running"><span className="pulse-dot"/>{counts.running} running</span> : null}
        {counts.awaiting  ? <span className="tsb-pill st-awaiting">{counts.awaiting} awaiting</span> : null}
        {counts.success   ? <span className="tsb-pill st-success">{counts.success} done</span> : null}
        {counts.idle      ? <span className="tsb-pill st-idle">{counts.idle} idle</span> : null}
        {counts.error     ? <span className="tsb-pill st-error">{counts.error} error</span> : null}
      </div>
      <div className="tsb-r">
        <LayoutSwitcher layout={layout} setLayout={setLayout}/>
      </div>
    </div>
  );
}

function LayoutSwitcher({ layout, setLayout }) {
  const opts = [
    { id:"1",   icon:<rect x="3" y="3" width="18" height="18" rx="2"/> },
    { id:"2v",  icon:<g><rect x="3" y="3" width="8" height="18" rx="2"/><rect x="13" y="3" width="8" height="18" rx="2"/></g> },
    { id:"2h",  icon:<g><rect x="3" y="3" width="18" height="8" rx="2"/><rect x="3" y="13" width="18" height="8" rx="2"/></g> },
    { id:"2x2", icon:<g><rect x="3" y="3" width="8" height="8" rx="2"/><rect x="13" y="3" width="8" height="8" rx="2"/><rect x="3" y="13" width="8" height="8" rx="2"/><rect x="13" y="13" width="8" height="8" rx="2"/></g> },
    { id:"3x4", icon:<g><rect x="3" y="3" width="5" height="8" rx="1.2"/><rect x="9.5" y="3" width="5" height="8" rx="1.2"/><rect x="16" y="3" width="5" height="8" rx="1.2"/><rect x="3" y="13" width="5" height="8" rx="1.2"/><rect x="9.5" y="13" width="5" height="8" rx="1.2"/><rect x="16" y="13" width="5" height="8" rx="1.2"/></g> },
  ];
  return (
    <div className="layout-switcher">
      {opts.map(o => (
        <button key={o.id} className={`ls-btn${layout===o.id?" active":""}`}
                onClick={()=>setLayout(o.id)} title={o.id}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none"
               stroke="currentColor" strokeWidth="1.75">{o.icon}</svg>
        </button>
      ))}
    </div>
  );
}

/* Agent icons — minimal mark per provider */
function AgentIcon({ kind, accent }) {
  const c = `var(--agent-${accent})`;
  if (kind === "claude") return (
    <svg width="14" height="14" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9" fill="none" stroke={c} strokeWidth="1.6"/><path d="M8 9 L12 15 L16 9" stroke={c} strokeWidth="1.6" fill="none" strokeLinecap="round" strokeLinejoin="round"/></svg>
  );
  if (kind === "openai") return (
    <svg width="14" height="14" viewBox="0 0 24 24"><path d="M12 3 L20 8 V16 L12 21 L4 16 V8 Z" fill="none" stroke={c} strokeWidth="1.6" strokeLinejoin="round"/></svg>
  );
  if (kind === "gemini") return (
    <svg width="14" height="14" viewBox="0 0 24 24"><path d="M12 2 L14 10 L22 12 L14 14 L12 22 L10 14 L2 12 L10 10 Z" fill={c}/></svg>
  );
  return (
    <svg width="14" height="14" viewBox="0 0 24 24"><path d="M5 9 L9 12 L5 15 M11 16 L17 16" stroke={c} strokeWidth="1.6" fill="none" strokeLinecap="round" strokeLinejoin="round"/></svg>
  );
}

window.TerminalRoute = TerminalRoute;
