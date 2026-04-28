/* Supporting routes — Agents, Runs, MCP marketplace, Settings.
   All real (use NeuronData fixtures), but lighter detail than canvas. */
/* global React */
const { useState } = React;

/* ---------- Agents ---------- */
function AgentsRoute() {
  const data = window.NeuronData;
  return (
    <div className="route route-agents">
      <div className="route-grid">
        {data.agents.map(a => (
          <div key={a.id} className="agent-card">
            <div className="agent-card-head">
              <div className="agent-avatar"><NodeGlyph kind="llm" size={22}/></div>
              <div>
                <div className="agent-name">{a.name}</div>
                <div className="agent-model">{a.model} · temp {a.temp}</div>
              </div>
              <div className="agent-spacer"/>
              <span className="pill st-ok">ready</span>
            </div>
            <div className="agent-role">{a.role}</div>
            <div className="agent-foot">
              <button className="btn ghost sm"><NIcon name="copy" size={12}/><span>Duplicate</span></button>
              <button className="btn ghost sm"><span>Open</span><NIcon name="chevronR" size={12}/></button>
            </div>
          </div>
        ))}
        <div className="agent-card add">
          <NIcon name="plus" size={22}/><span>New agent</span>
        </div>
      </div>
    </div>
  );
}

/* ---------- Runs ---------- */
function RunsRoute() {
  const data = window.NeuronData;
  const [filter, setFilter] = useState("all");
  const filtered = filter === "all" ? data.runs : data.runs.filter(r => r.status === filter);
  return (
    <div className="route route-runs">
      <div className="runs-toolbar">
        <div className="chip-row">
          {["all","running","success","error"].map(f=>(
            <button key={f} className={`chip${filter===f?" active":""}`} onClick={()=>setFilter(f)}>
              {f === "running" && <span className="pulse-dot"/>}
              {f}
            </button>
          ))}
        </div>
        <div className="runs-stats">
          <span><b>{data.runs.length}</b> runs</span>
          <span><b>$0.049</b> today</span>
          <span><b>20.1k</b> tokens</span>
        </div>
      </div>
      <table className="runs-table">
        <thead>
          <tr><th>id</th><th>workflow</th><th>started</th><th>duration</th><th>tokens</th><th>cost</th><th>status</th></tr>
        </thead>
        <tbody>
          {filtered.map(r => (
            <tr key={r.id}>
              <td className="mono">{r.id}</td>
              <td>{r.workflow}</td>
              <td className="mute">{r.started}</td>
              <td>{(r.dur/1000).toFixed(2)}s</td>
              <td>{r.tokens.toLocaleString()}</td>
              <td>${r.cost.toFixed(4)}</td>
              <td>
                <span className={`pill st-${r.status === "success" ? "ok" : r.status === "running" ? "running" : "error"}`}>
                  {r.status === "running" && <span className="pulse-dot"/>}{r.status}
                </span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/* ---------- MCP marketplace ---------- */
function MCPRoute() {
  const data = window.NeuronData;
  const featured = data.servers.filter(s => s.featured);
  const all = data.servers;
  return (
    <div className="route route-mcp">
      <div className="mcp-search">
        <NIcon name="search" size={16}/>
        <input placeholder="Search 48 servers…"/>
        <div className="chip-row mcp-chips">
          {["all","official","community","installed"].map((c,i)=>(
            <button key={c} className={`chip${i===0?" active":""}`}>{c}</button>
          ))}
        </div>
      </div>

      <h3 className="route-section-title">Featured</h3>
      <div className="mcp-featured">
        {featured.map(s => (
          <ServerCard key={s.id} s={s} featured/>
        ))}
      </div>

      <h3 className="route-section-title">All servers</h3>
      <div className="mcp-list">
        {all.map(s => <ServerRow key={s.id} s={s}/>)}
      </div>
    </div>
  );
}

function ServerCard({ s, featured }) {
  return (
    <div className={`server-card${featured?" featured":""}`}>
      <div className="server-card-head">
        <div className="server-icon">
          <NodeGlyph kind="mcp" size={22}/>
        </div>
        <div>
          <div className="server-name">{s.name}</div>
          <div className="server-by">{s.by}</div>
        </div>
        <div className="server-spacer"/>
        {s.installed
          ? <span className="pill st-installed">installed</span>
          : <button className="btn primary sm"><NIcon name="download" size={11}/><span>Install</span></button>}
      </div>
      <div className="server-desc">{s.desc}</div>
      <div className="server-foot">
        <span><NIcon name="download" size={11}/> {(s.installs/1000).toFixed(1)}k</span>
        <span><NIcon name="star" size={11}/> {s.rating}</span>
      </div>
    </div>
  );
}

function ServerRow({ s }) {
  return (
    <div className="server-row">
      <div className="server-icon sm"><NodeGlyph kind="mcp" size={16}/></div>
      <div className="sr-meta">
        <div className="sr-line">
          <span className="sr-name">{s.name}</span>
          <span className="sr-by">{s.by}</span>
        </div>
        <div className="sr-desc">{s.desc}</div>
      </div>
      <div className="sr-stats">
        <span>{(s.installs/1000).toFixed(1)}k</span>
        <span>★ {s.rating}</span>
      </div>
      {s.installed
        ? <span className="pill st-installed">installed</span>
        : <button className="btn ghost sm">Install</button>}
    </div>
  );
}

/* ---------- Settings ---------- */
function SettingsRoute() {
  const sections = [
    { id:"account",     label:"Account",      icon:"bot" },
    { id:"appearance",  label:"Appearance",   icon:"sun" },
    { id:"workflows",   label:"Workflows",    icon:"workflow" },
    { id:"agents",      label:"Agents",       icon:"sparkles" },
    { id:"models",      label:"Models",       icon:"zap" },
    { id:"mcp",         label:"MCP",          icon:"store" },
    { id:"keys",        label:"Keys",         icon:"plug" },
    { id:"data",        label:"Data",         icon:"layers" },
  ];
  const [active, setActive] = useState("appearance");
  return (
    <div className="route route-settings">
      <nav className="settings-nav">
        {sections.map(s => (
          <button key={s.id} className={`set-item${active===s.id?" active":""}`} onClick={()=>setActive(s.id)}>
            <NIcon name={s.icon} size={15}/>
            <span>{s.label}</span>
          </button>
        ))}
      </nav>
      <div className="settings-pane">
        {active === "appearance" && <AppearancePane/>}
        {active !== "appearance" && (
          <div className="set-empty">
            <h2 className="text-h2" style={{marginTop:0}}>{sections.find(s=>s.id===active).label}</h2>
            <p className="text-muted">Settings for this section.</p>
          </div>
        )}
      </div>
    </div>
  );
}

function AppearancePane() {
  return (
    <>
      <h2 className="text-h2" style={{marginTop:0}}>Appearance</h2>
      <p className="text-muted">Colors, density, and motion. Changes apply instantly.</p>

      <div className="set-card">
        <div className="set-row">
          <div>
            <div className="set-row-title">Theme</div>
            <div className="set-row-sub">Match the OS or pick one.</div>
          </div>
          <div className="seg">
            <button>Light</button><button className="active">Dark</button><button>System</button>
          </div>
        </div>
        <div className="set-row">
          <div>
            <div className="set-row-title">Accent</div>
            <div className="set-row-sub">Used on selection, focus, and Synapse Violet surfaces.</div>
          </div>
          <div className="swatches">
            {["#a874d6","#7aa6f0","#e0a85b","#7ad6c8","#d678a6"].map((c,i)=>(
              <button key={c} className={`sw${i===0?" active":""}`} style={{background:c}}/>
            ))}
          </div>
        </div>
        <div className="set-row">
          <div>
            <div className="set-row-title">Density</div>
            <div className="set-row-sub">Comfortable spacing or tighter rows.</div>
          </div>
          <div className="seg">
            <button className="active">Comfortable</button><button>Compact</button>
          </div>
        </div>
        <div className="set-row">
          <div>
            <div className="set-row-title">Motion</div>
            <div className="set-row-sub">Edge dataflow, node pulse, glow shimmer.</div>
          </div>
          <div className="seg">
            <button className="active">Full</button><button>Reduced</button><button>Off</button>
          </div>
        </div>
      </div>
    </>
  );
}

window.AgentsRoute = AgentsRoute;
window.RunsRoute = RunsRoute;
window.MCPRoute = MCPRoute;
window.SettingsRoute = SettingsRoute;
