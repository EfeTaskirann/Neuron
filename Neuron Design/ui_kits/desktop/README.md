# Neuron Desktop UI Kit

A click-thru recreation of the Neuron desktop app shell. The layout is the Arc-vari 3-column grid: sidebar (260) · main content (1fr) · right inspector (400, collapsible).

## Routes (tabs in this kit)

- **Workflows** — the canvas at `/workflows/demo` with mock React-Flow-style nodes and an animated edge
- **Runs** — Run Inspector with hierarchical span waterfall + selected-span sheet
- **Marketplace** — MCP server cards (grid)
- **Settings** — Things-3-hizası two-column form, Appearance section live (theme + density toggles)

## Files

- `index.html` — mounts the shell, click between tabs
- `Shell.jsx` — sidebar + topbar + main + inspector
- `Canvas.jsx` — workflow canvas with custom nodes and animated edge
- `RunInspector.jsx` — span waterfall
- `Marketplace.jsx` — MCP grid
- `Settings.jsx` — settings form
- `Primitives.jsx` — Button, Badge, KbdHint, StatusDot, Icon helpers

This is a visual recreation, not a working app. State is local. Open `index.html` in the preview.
