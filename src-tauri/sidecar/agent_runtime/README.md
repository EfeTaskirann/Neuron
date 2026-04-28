# Neuron agent runtime (Python sidecar)

LangGraph-based execution sidecar for Neuron, supervised by the Rust
side at `src-tauri/src/sidecar/agent.rs`. Spawned at app startup,
killed on app shutdown. Communicates with the supervisor over stdio
using 4-byte big-endian length-prefixed JSON frames.

This README covers **developer setup**. End users will get a bundled
sidecar in Week 3 (PyOxidizer).

## One-time setup

1. Install `uv` (pip-installable):

   ```sh
   pip install --user uv
   ```

   On Windows, add `%APPDATA%\Python\Python311\Scripts` (or the path
   `pip` printed) to `PATH` so `uv` is callable.

2. Sync dependencies (creates `.venv/` in this directory):

   ```sh
   cd src-tauri/sidecar/agent_runtime
   uv sync
   ```

3. Configure API keys in the OS keychain (Charter §"Hard constraints"
   #2 forbids plaintext keys):

   ```sh
   # Anthropic key for the planner / reasoner nodes
   keyring set neuron anthropic
   # OpenAI key (optional — workflow falls through if absent)
   keyring set neuron openai
   ```

   On Windows, `keyring` uses Windows Credential Manager. You can also
   set keys interactively via `python -c "import keyring; keyring.set_password('neuron', 'anthropic', '...')"`.

   If neither key is configured, the workflow still starts, but the
   first LLM node fails fast and surfaces an `attrs.error='no_api_key'`
   span so the UI can show a "Configure API keys" CTA. The run
   terminates with `status='error'`.

## Running tests

```sh
cd src-tauri/sidecar/agent_runtime
uv run pytest
```

Tests are **hermetic** — they mock the LLM client and never call
external services or read OS keychain.

## Stdio protocol (length-prefixed JSON-RPC)

```
+---------+------------------+
| 4 bytes | UTF-8 JSON body  |
| u32 BE  | (length bytes)   |
+---------+------------------+
```

Inbound (Rust → Python):

```jsonc
{ "method": "run.start", "params": { "workflowId": "daily-summary", "runId": "r-..." } }
{ "method": "shutdown" }
```

Outbound (Python → Rust):

```jsonc
{ "event": "span.created", "runId": "r-...", "span": { ... } }
{ "event": "span.updated", "runId": "r-...", "span": { ... } }
{ "event": "span.closed",  "runId": "r-...", "span": { ... } }
{ "event": "run.completed","runId": "r-...", "status": "success" | "error", "error": "..." }
```

`Span` payload mirrors `src-tauri/src/models.rs::Span` exactly:

```jsonc
{
  "id": "s-...",
  "runId": "r-...",
  "parentSpanId": null,
  "name": "planner",
  "type": "llm",
  "t0Ms": 12345,
  "durationMs": 450,
  "attrsJson": "{}",
  "prompt": "...",
  "response": "...",
  "isRunning": false
}
```

Field name parity with the WP-W2-03 model (`Span.span_type` →
`type`, `t0_ms` → `t0Ms`, etc.) is enforced by serde renames on the
Rust side. The sidecar emits camelCase by convention.

## Demo workflow: daily-summary

Hardcoded LangGraph topology (one workflow ships in Week 2):

```
planner (llm) → fetch_docs (tool) ─┐
              → search_web (tool) ─┴→ reasoner (llm) → human_approval (human)
```

`fetch_docs` and `search_web` are **mock tool nodes** until WP-W2-05
wires real MCP. `human_approval` is a no-op span that closes
immediately — Week 3 introduces the real approval UI.
