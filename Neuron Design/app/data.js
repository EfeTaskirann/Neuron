/* Mock fixtures for the Neuron desktop app prototype.
   Sets window.NeuronData — read by shell.jsx, routes.jsx, canvas.jsx, inspector.jsx. */

window.NeuronData = {
  user: {
    initials: "ET",
    name: "Efe Taşkıran",
  },

  workspace: {
    name: "Personal",
    count: 3,
  },

  /* ---------- Agents (Agents route + canvas references) ---------- */
  agents: [
    {
      id: "a1",
      name: "Planner",
      model: "gpt-4o",
      temp: 0.4,
      role: "Breaks the goal into ordered subtasks. Hands off to tools and writers.",
    },
    {
      id: "a2",
      name: "Reasoner",
      model: "claude-opus-4-7",
      temp: 0.3,
      role: "Synthesizes evidence from tools and drafts the final summary.",
    },
    {
      id: "a3",
      name: "Researcher",
      model: "gpt-4o-mini",
      temp: 0.6,
      role: "Searches the web and pulls relevant snippets back to the planner.",
    },
    {
      id: "a4",
      name: "Reviewer",
      model: "claude-sonnet-4-6",
      temp: 0.2,
      role: "Critiques drafts, flags missing context, requests revisions.",
    },
  ],

  /* ---------- Runs (Runs route — table) ---------- */
  runs: [
    { id: "r-9c2f1a", workflow: "Daily summary", started: "2 min ago",  dur: 2400,  tokens:  3824, cost: 0.0124, status: "running" },
    { id: "r-8e1c42", workflow: "Daily summary", started: "11 min ago", dur: 3120,  tokens:  4280, cost: 0.0148, status: "success" },
    { id: "r-7b04df", workflow: "PR review",     started: "32 min ago", dur: 5240,  tokens:  6190, cost: 0.0212, status: "success" },
    { id: "r-6a23e8", workflow: "Email triage",  started: "1 hr ago",   dur:  890,  tokens:  1280, cost: 0.0042, status: "success" },
    { id: "r-5910fc", workflow: "Daily summary", started: "2 hr ago",   dur: 2680,  tokens:  3940, cost: 0.0131, status: "error"   },
    { id: "r-48ad21", workflow: "PR review",     started: "3 hr ago",   dur: 4870,  tokens:  5820, cost: 0.0198, status: "success" },
    { id: "r-37cc09", workflow: "Email triage",  started: "5 hr ago",   dur:  710,  tokens:  1120, cost: 0.0036, status: "success" },
    { id: "r-2683b4", workflow: "Daily summary", started: "8 hr ago",   dur: 2530,  tokens:  3680, cost: 0.0119, status: "success" },
    { id: "r-1592a7", workflow: "PR review",     started: "12 hr ago",  dur: 5410,  tokens:  6420, cost: 0.0224, status: "success" },
    { id: "r-04b8d3", workflow: "Email triage",  started: "1 d ago",    dur:  680,  tokens:  1040, cost: 0.0033, status: "error"   },
  ],

  /* ---------- MCP servers (MCP route — featured + list) ---------- */
  servers: [
    { id: "filesystem", name: "Filesystem", by: "Anthropic",  desc: "Read, write, and search the local filesystem from any agent. Sandboxed per-workspace.",       installs: 12400, rating: 4.9, featured: true,  installed: true  },
    { id: "github",     name: "GitHub",     by: "GitHub",     desc: "Issues, PRs, and code search via the GitHub REST + GraphQL APIs.",                              installs: 21000, rating: 4.9, featured: true,  installed: true  },
    { id: "postgres",   name: "PostgreSQL", by: "Anthropic",  desc: "Query relational databases with role-scoped credentials. Read-only by default.",                installs:  8100, rating: 4.8, featured: false, installed: false },
    { id: "browser",    name: "Browser",    by: "Anthropic",  desc: "Headless Chromium with screenshot, scroll, and click actions. Locked to allowlist.",            installs:  6700, rating: 4.6, featured: false, installed: false },
    { id: "slack",      name: "Slack",      by: "Slack",      desc: "Send and read messages, create threads, manage channels in your workspace.",                    installs:  3200, rating: 4.5, featured: false, installed: true  },
    { id: "vector-db",  name: "Vector DB",  by: "Qdrant",     desc: "Embed and retrieve from Qdrant, pgvector, or Chroma with one config block.",                    installs:  4400, rating: 4.7, featured: false, installed: false },
    { id: "linear",     name: "Linear",     by: "Linear",     desc: "Create issues, search projects, update statuses across your Linear workspace.",                 installs:  2900, rating: 4.6, featured: false, installed: false },
    { id: "notion",     name: "Notion",     by: "Notion",     desc: "Read and write pages, databases, and blocks with token-scoped access.",                         installs:  5600, rating: 4.7, featured: false, installed: false },
    { id: "stripe",     name: "Stripe",     by: "Stripe",     desc: "Inspect customers, charges, and subscriptions. Live and test mode supported.",                  installs:  1800, rating: 4.8, featured: false, installed: false },
    { id: "sentry",     name: "Sentry",     by: "Sentry",     desc: "Pull recent errors, breadcrumbs, and release health into the agent context.",                   installs:  1400, rating: 4.5, featured: false, installed: false },
    { id: "figma",      name: "Figma",      by: "Figma",      desc: "Read frames and components from a file URL; export PNG/SVG to the workspace.",                  installs:  3700, rating: 4.4, featured: false, installed: false },
    { id: "memory",     name: "Memory",     by: "Anthropic",  desc: "Long-term key-value store scoped to the workspace; safe for cross-run continuity.",             installs:  2100, rating: 4.6, featured: false, installed: false },
  ],
};
