/* App-shell layout — wraps Sidebar + Topbar + Main + (optional) Inspector.
   Uses CSS grid: 240px | 1fr | (320px optional). */
/* global React */
const { useState } = React;

function AppShell({ route, setRoute, children, inspector }) {
  const [collapsed, setCollapsed] = useState(false);
  return (
    <div className={`app-shell${collapsed ? " collapsed":""}${inspector ? " has-inspector":""}`}>
      <Sidebar route={route} setRoute={setRoute} collapsed={collapsed} onToggle={() => setCollapsed(c => !c)} />
      <Topbar route={route} />
      <main className="app-main">{children}</main>
      {inspector ? <aside className="app-inspector">{inspector}</aside> : null}
    </div>
  );
}

const NAV = [
  { id: "canvas",   label: "Workflow",    icon: "workflow" },
  { id: "terminal", label: "Terminal",    icon: "server" },
  { id: "agents",   label: "Agents",      icon: "bot" },
  { id: "runs",     label: "Runs",        icon: "activity" },
  { id: "mcp",      label: "MCP",         icon: "store" },
  { id: "settings", label: "Settings",    icon: "settings" },
];

function Sidebar({ route, setRoute, collapsed, onToggle }) {
  const data = window.NeuronData;
  return (
    <nav className="sidebar">
      <div className="sb-brand" onClick={onToggle} role="button" title="Toggle sidebar">
        <Brandmark size={28}/>
        {!collapsed && <span className="sb-wordmark">Neuron</span>}
      </div>

      {!collapsed && (
        <div className="sb-workspace">
          <div className="sb-ws-avatar">{data.user.initials}</div>
          <div className="sb-ws-meta">
            <div className="sb-ws-name">{data.workspace.name}</div>
            <div className="sb-ws-sub">{data.workspace.count} workflows</div>
          </div>
          <NIcon name="chevron" size={14} style={{opacity:0.5}}/>
        </div>
      )}

      <ul className="sb-nav">
        {NAV.map(item => (
          <li key={item.id}
              className={`sb-item${route === item.id ? " active":""}`}
              onClick={() => setRoute(item.id)}>
            <NIcon name={item.icon} size={18}/>
            {!collapsed && <span>{item.label}</span>}
            {!collapsed && route === item.id && <span className="sb-dot"/>}
          </li>
        ))}
      </ul>

      {!collapsed && (
        <div className="sb-section">
          <div className="sb-section-title">Workflows</div>
          {["Daily summary","PR review","Email triage"].map((n,i)=>(
            <div key={n} className={`sb-leaf${i===0?" active":""}`}>
              <span className="sb-leaf-dot" style={{background: i===0 ? "var(--syn-running)" : "var(--neuron-slate-500)"}}/>
              <span>{n}</span>
            </div>
          ))}
        </div>
      )}

      <div className="sb-foot">
        <div className="sb-foot-avatar">{data.user.initials}</div>
        {!collapsed && (
          <>
            <div className="sb-foot-meta">
              <div className="sb-foot-name">{data.user.name}</div>
              <div className="sb-foot-sub">Free plan</div>
            </div>
            <NIcon name="chevron" size={14} style={{opacity:0.5}}/>
          </>
        )}
      </div>
    </nav>
  );
}

function Topbar({ route }) {
  const titles = {
    canvas: "Daily summary",
    terminal: "Terminal",
    agents: "Agents",
    runs: "Runs",
    mcp: "MCP marketplace",
    settings: "Settings",
  };
  const subs = {
    canvas: "Personal · saved 2 min ago",
    terminal: "personal · 4 panes · 2x2",
    agents: "3 agents",
    runs: "Last 24 hours",
    mcp: "48 servers · 3 installed",
    settings: "",
  };
  return (
    <header className="topbar">
      <div className="topbar-l">
        <div className="topbar-crumb">
          <span className="crumb-mute">Personal</span>
          <NIcon name="chevronR" size={12} style={{opacity:0.4}}/>
          <span>{titles[route]}</span>
        </div>
        {subs[route] && <div className="topbar-sub">{subs[route]}</div>}
      </div>

      <div className="topbar-search">
        <NIcon name="search" size={14}/>
        <input placeholder="Search workflows, agents, servers…"/>
        <kbd>⌘K</kbd>
      </div>

      <div className="topbar-r">
        {route === "canvas" && (
          <>
            <button className="btn ghost"><NIcon name="clock" size={14}/><span>History</span></button>
            <button className="btn primary"><NIcon name="play" size={12}/><span>Run</span></button>
          </>
        )}
        {route === "terminal" && (
          <>
            <button className="btn ghost"><NIcon name="search" size={13}/><span>Search panes</span></button>
            <button className="btn primary"><NIcon name="plus" size={13}/><span>New pane</span></button>
          </>
        )}
        {route !== "canvas" && route !== "terminal" && (
          <button className="btn primary"><NIcon name="plus" size={14}/><span>New</span></button>
        )}
      </div>
    </header>
  );
}

window.AppShell = AppShell;
