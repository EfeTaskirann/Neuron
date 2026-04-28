/* global React, NeuronUI */
const { useState } = React;
const { Icon, Badge, Button } = NeuronUI;

const ITEMS = [
  { name: "Filesystem", desc: "Read, write, and search the local filesystem from any agent. Sandboxed per-workspace.", featured: true, official: true, stars: 4.9, installs: "12.4k" },
  { name: "PostgreSQL", desc: "Query relational databases with role-scoped credentials. Read-only by default.", official: true, stars: 4.8, installs: "8.1k" },
  { name: "GitHub", desc: "Issues, PRs, and code search via the GitHub REST + GraphQL APIs.", official: true, stars: 4.9, installs: "21.0k" },
  { name: "Browser", desc: "Headless Chromium with screenshot, scroll, and click actions. Locked to allowlist.", stars: 4.6, installs: "6.7k" },
  { name: "Slack", desc: "Send and read messages, create threads, manage channels in your workspace.", stars: 4.5, installs: "3.2k" },
  { name: "Vector DB", desc: "Embed and retrieve from Qdrant, pgvector, or Chroma with one config block.", stars: 4.7, installs: "4.4k" },
];

const Marketplace = () => {
  const [filter, setFilter] = useState("All");
  return (
    <div style={{ padding: "20px 24px", overflow: "auto", height: "100%" }}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 18 }}>
        <div>
          <div style={{ fontSize: 24, fontWeight: 600, letterSpacing: "-0.01em" }}>MCP Marketplace</div>
          <div style={{ fontSize: 13, color: "var(--muted-foreground)", marginTop: 2 }}>Connect any tool. Sandboxed per-workspace.</div>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <div style={{ position: "relative" }}>
            <Icon name="search" size={14} color="var(--muted-foreground)" />
            <input placeholder="Search 800+ servers" style={{
              position: "absolute", inset: 0, paddingLeft: 28, paddingRight: 12,
              width: 260, height: 32, borderRadius: 8, background: "var(--input)",
              border: "1px solid var(--border)", color: "var(--foreground)",
              fontSize: 13, fontFamily: "var(--font-sans)", outline: "none",
            }} />
            <div style={{ position: "absolute", left: 10, top: 9, pointerEvents: "none" }}><Icon name="search" size={14} color="var(--muted-foreground)" /></div>
          </div>
        </div>
      </div>
      <div style={{ display: "flex", gap: 6, marginBottom: 18 }}>
        {["All", "Official", "Community", "Local"].map(f => (
          <button key={f} onClick={() => setFilter(f)} style={{
            height: 28, padding: "0 12px", borderRadius: 9999, fontSize: 12, fontWeight: 500,
            background: filter === f ? "var(--accent)" : "transparent",
            color: filter === f ? "var(--accent-foreground)" : "var(--muted-foreground)",
            border: filter === f ? "1px solid transparent" : "1px solid var(--border)",
            cursor: "pointer", fontFamily: "var(--font-sans)",
          }}>{f}</button>
        ))}
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: 16 }}>
        {ITEMS.map(item => (
          <div key={item.name} style={{
            borderRadius: 24, background: "var(--card)", border: "1px solid var(--border)",
            padding: 16, boxShadow: "var(--shadow-sm)", transition: "all 160ms var(--ease-out)",
          }}>
            <div style={{ display: "flex", alignItems: "flex-start", gap: 12 }}>
              <div style={{
                width: 44, height: 44, borderRadius: 12,
                background: "linear-gradient(135deg, var(--neuron-midnight-800), var(--neuron-midnight-950))",
                display: "grid", placeItems: "center", color: "var(--neuron-violet-300)", flex: "none",
              }}>
                <Icon name="server" size={22} />
              </div>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <span style={{ fontSize: 14, fontWeight: 600 }}>{item.name}</span>
                  {item.featured && <Badge variant="featured">Featured</Badge>}
                </div>
                <div style={{ fontSize: 12, color: "var(--muted-foreground)", marginTop: 4, lineHeight: 1.45,
                  display: "-webkit-box", WebkitLineClamp: 2, WebkitBoxOrient: "vertical", overflow: "hidden" }}>{item.desc}</div>
              </div>
            </div>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginTop: 14 }}>
              <div style={{ fontSize: 11, color: "var(--muted-foreground)", display: "flex", gap: 8 }}>
                <span>★ {item.stars}</span><span>·</span><span>{item.installs}</span>
                {item.official && <><span>·</span><span>Official</span></>}
              </div>
              <Button size="sm">Install</Button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
};

window.NeuronMarketplace = Marketplace;
