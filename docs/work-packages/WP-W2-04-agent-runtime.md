---
id: WP-W2-04
title: LangGraph agent runtime (Python sidecar)
owner: TBD
status: not-started
depends-on: [WP-W2-03]
acceptance-gate: "runs:create starts a real LangGraph workflow; spans persist; run completes"
---

## Goal

Replace the WP-W2-03 stub for `runs:create` with a real execution: spawn a Python LangGraph sidecar, send the workflow definition, stream span events back, persist them to `runs_spans`. Ship one demo workflow ("Daily summary") hardcoded.

## Scope

- Add a Python sidecar at `src-tauri/sidecar/agent_runtime/`:
  - `pyproject.toml` (uv-managed): `langgraph`, `langchain-anthropic`, `langchain-openai`, `pydantic`
  - `agent_runtime/__main__.py` — JSON-RPC over stdio, accepts `run.start { workflow, run_id }`, emits `span.created`, `span.updated`, `span.closed`, `run.completed`
- Bundle: Week 2 ships with system Python 3.11+ requirement. Document install in README. PyOxidizer in Week 3.
- Rust side: `src-tauri/src/sidecar/agent.rs`:
  - `spawn_runtime(app: &AppHandle) -> SidecarHandle` — spawns Python child, holds stdin/stdout
  - `start_run(handle: &SidecarHandle, workflow: Workflow, run_id: &str) -> Result<()>`
  - Async stream consumer that converts JSON events → DB writes via WP-02 pool
- Demo workflow "daily-summary" hardcoded in Python:
  - Planner (LLM) → fetch_docs (tool) + search_web (tool) → Reasoner (LLM) → human approval node
  - Tool nodes for now use mock implementations (return canned strings); WP-05 wires real MCP tools
- API key handling: read Anthropic / OpenAI keys from OS keychain via `keyring` Python lib
- Sidecar lifecycle: spawn at app startup, kill on app shutdown (Tauri `RunEvent::Exit` hook)

## Out of scope

- Real MCP tool calls (WP-W2-05)
- Multi-workflow editor (Week 3)
- Streaming partial LLM responses to UI (Week 3)
- Cancel signal mid-LLM-call (best effort: kill the sidecar's run task; do NOT kill the whole sidecar)

## Acceptance criteria

- [ ] `await invoke('runs:create', { workflowId: 'daily-summary' })` returns a run id
- [ ] Within ~5s, spans appear in `runs_spans` for the run (planner → tools → reasoner)
- [ ] Run terminates with `status='success'` (or `'error'` if API keys missing — gracefully)
- [ ] `await invoke('runs:get', { id })` returns run + spans matching the frontend mock shape
- [ ] Sidecar process is killed on app shutdown (no orphan `python` process on next launch — verify with task manager)
- [ ] Missing API key surfaces as a span with `type='llm', is_running=0, attrs.error='no_api_key'` and run `status='error'`

## Verification commands

```bash
# 1. ensure python+deps installed
cd src-tauri/sidecar/agent_runtime && uv sync && cd -

# 2. unit test the sidecar standalone
cd src-tauri/sidecar/agent_runtime && uv run pytest && cd -

# 3. integration via Rust
cargo test --manifest-path src-tauri/Cargo.toml -- sidecar::agent

# 4. manual: pnpm tauri dev, devtools console
#   const { id } = await invoke('runs:create', { workflowId: 'daily-summary' });
#   await new Promise(r => setTimeout(r, 5000));
#   const { spans } = await invoke('runs:get', { id });
#   spans.length >= 5  // planner + 2 tools + reasoner + human-approval
```

## Notes / risks

- LangGraph version churn: pin to a specific version (e.g., `langgraph==0.2.x`) in `pyproject.toml`.
- Sidecar startup time (Python import): ~1-2s on first call. Display "Starting agent" pill on first run.
- Stdio framing: use length-prefixed JSON (4-byte big-endian length + UTF-8 JSON body) to avoid line-buffering issues.
- API keys NEVER appear in logs. Sidecar reads via `keyring.get_password('neuron', 'anthropic')`.
- If `keyring` doesn't have a key, surface a structured error so UI can show a "Configure API keys" CTA.
- Test with both Anthropic and OpenAI providers — workflow should survive provider swap.

## Sub-agent reminders

- Read NEURON_TERMINAL_REPORT.md if any pane interaction is needed (NOT in this WP — terminal is WP-06)
- Do NOT add LangChain `ChatModel` callbacks for span emission; emit events explicitly from the LangGraph node so the schema is clean
- Do NOT cache LLM responses in this WP — Week 3 adds a cache layer
