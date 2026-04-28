/* Terminal mock — Daily summary scenario, 2x2 layout, 4 panes.
   Each pane has: agent manifest, status, scrollback (lines with type), command blocks (OSC 133). */
window.NeuronTerminalData = {
  workspace: { name: "personal", panes: 4, layout: "2x2" },

  agents: {
    "claude-code": { name: "Claude Code", accent: "violet", icon: "claude" },
    "codex":       { name: "Codex CLI",   accent: "emerald", icon: "openai" },
    "gemini":      { name: "Gemini CLI",  accent: "amber",   icon: "gemini" },
    "shell":       { name: "zsh",         accent: "slate",   icon: "shell" },
  },

  panes: [
    {
      id: "p1", agent: "claude-code", role: "builder",
      cwd: "~/projects/neuron/src/features/canvas",
      status: "awaiting_approval", pid: 8421, uptime: "12m 04s",
      tokensIn: 18400, tokensOut: 6200, costUsd: 0.0612,
      // approval banner data
      approval: { tool: "write_file", target: "src/components/Button.tsx", added: 47, removed: 12 },
      // scrollback — { kind, text }; kind ∈ prompt|command|out|err|tool|thinking|sys
      lines: [
        { k:"sys", text:"Claude Code v0.8.4 · session 9c2f" },
        { k:"sys", text:"workspace: ~/projects/neuron · branch: feat/canvas-edges" },
        { k:"prompt", text:"" },
        { k:"command", text:"refactor the edge animation to use animateMotion" },
        { k:"thinking", text:"Reading WorkflowCanvas.tsx, EdgeLayer.tsx…" },
        { k:"tool", text:"read_file  src/features/canvas/EdgeLayer.tsx  (218 lines)" },
        { k:"tool", text:"read_file  src/features/canvas/WorkflowCanvas.tsx  (412 lines)" },
        { k:"out", text:"I see the current edges use CSS keyframes. Switching to <animateMotion>" },
        { k:"out", text:"will let us bind to the actual path data, which removes the manual" },
        { k:"out", text:"offset math. Drafting the change now." },
        { k:"thinking", text:"Drafting patch · src/components/Button.tsx" },
      ],
      blocks: [{ id:"b1", cmd:"refactor the edge animation to use animateMotion", exit:null, dur:null, status:"running" }],
    },

    {
      id: "p2", agent: "codex", role: "reviewer",
      cwd: "~/projects/neuron",
      status: "running", pid: 8422, uptime: "12m 04s",
      tokensIn: 9100, tokensOut: 2800, costUsd: 0.0184,
      lines: [
        { k:"sys", text:"codex v1.2.0 · model gpt-4o" },
        { k:"prompt", text:"" },
        { k:"command", text:"review the diff on feat/canvas-edges" },
        { k:"thinking", text:"thinking…" },
        { k:"tool", text:"git.diff   feat/canvas-edges..main   (3 files, +180 -94)" },
        { k:"out", text:"Reviewed 3 files. Two findings:" },
        { k:"out", text:"  · EdgeLayer.tsx:142 — animateMotion path isn't memoized; will" },
        { k:"out", text:"    re-allocate every render. Wrap the path string in useMemo." },
        { k:"out", text:"  · WorkflowCanvas.tsx:88 — selected node z-index relies on" },
        { k:"out", text:"    DOM order; consider explicit z-index for clarity." },
        { k:"out", text:"Continuing analysis on token-cost overlay…" },
      ],
      blocks: [{ id:"b2", cmd:"review the diff on feat/canvas-edges", exit:null, dur:null, status:"running" }],
    },

    {
      id: "p3", agent: "shell", role: null,
      cwd: "~/projects/neuron",
      status: "success", pid: 8390, uptime: "14m 02s",
      tokensIn: null, tokensOut: null, costUsd: null,
      lines: [
        { k:"prompt", text:"~/projects/neuron $", inline:"pnpm dev" },
        { k:"out", text:"  vite v5.4.0  ready in 184 ms" },
        { k:"out", text:"  ➜  Local:   http://localhost:5173/" },
        { k:"out", text:"  ➜  Network: use --host to expose" },
        { k:"out", text:"" },
        { k:"out", text:"watching src/ for changes…" },
        { k:"prompt", text:"~/projects/neuron $", inline:"git status" },
        { k:"out", text:"On branch feat/canvas-edges" },
        { k:"out", text:"Changes not staged for commit:" },
        { k:"out", text:"  modified:   src/features/canvas/EdgeLayer.tsx" },
        { k:"out", text:"  modified:   src/features/canvas/WorkflowCanvas.tsx" },
        { k:"out", text:"" },
        { k:"prompt", text:"~/projects/neuron $", inline:"" },
      ],
      blocks: [
        { id:"sb1", cmd:"pnpm dev",    exit:0, dur:184,  status:"success" },
        { id:"sb2", cmd:"git status",  exit:0, dur:42,   status:"success" },
      ],
    },

    {
      id: "p4", agent: "gemini", role: "test-runner",
      cwd: "~/projects/neuron",
      status: "error", pid: 8425, uptime: "08m 11s",
      tokensIn: 4200, tokensOut: 1600, costUsd: 0.0046,
      lines: [
        { k:"sys", text:"gemini-cli v0.6.2 · model gemini-2.5-pro" },
        { k:"prompt", text:"" },
        { k:"command", text:"run the test suite and report failures" },
        { k:"tool", text:"shell.exec   pnpm test --reporter=verbose" },
        { k:"out", text:"  ✓ src/utils/path.test.ts (8)" },
        { k:"out", text:"  ✓ src/features/canvas/edges.test.ts (12)" },
        { k:"err", text:"  ✗ src/features/canvas/animation.test.ts (1 failed of 4)" },
        { k:"err", text:"    × animateMotion path memoization · expected 1, got 6" },
        { k:"err", text:"      Expected: useMemo cache hit on rerender" },
        { k:"err", text:"      Got: 6 path allocations across 6 frames" },
        { k:"out", text:"" },
        { k:"out", text:"Tests: 1 failed, 23 passed, 24 total — 1.84s" },
        { k:"out", text:"Reporting back to mailbox → reviewer (Codex)…" },
      ],
      blocks: [{ id:"b4", cmd:"run the test suite and report failures", exit:1, dur:1840, status:"error" }],
    },
  ],

  // mailbox — recent messages between panes
  mailbox: [
    { ts:"12m 02s", from:"p4", to:"p2", type:"task:failed",  summary:"animation.test.ts × memoization" },
    { ts:"11m 48s", from:"p2", to:"p1", type:"request:review", summary:"add useMemo to path string" },
    { ts:"11m 30s", from:"p1", to:"p2", type:"task:done",     summary:"draft patch ready · Button.tsx" },
  ],
};
