---
id: WP-W4-03
title: Per-agent event channel (`swarm:agent:{ws}:{id}:event`) — live UI feed
owner: TBD
status: not-started
depends-on: [WP-W4-02]
acceptance-gate: "New `SwarmAgentEvent` enum (Spawned / TurnStarted / AssistantText / ToolUse / Result / HelpRequest / Idle / Crashed) emitted on per-(workspace, agent) Tauri channels. Registry threads an `mpsc` sender into `PersistentSession::invoke_turn` so streaming `AssistantText` / `ToolUse` events fire as claude produces them. Frontend `useAgentEvents(workspaceId, agentId)` hook subscribes, returns ring-buffered tail (cap 200 per agent). `cargo test --lib` green; `pnpm test` green; `pnpm typecheck` green."
---

## Goal

Make every persistent agent's activity observable from the
frontend. W4-02 owns lifecycle but the UI can't see what's
happening inside a turn — only the bookend Idle / Running pill.
W4-03 adds the streaming substrate so the W4-04 grid panes can
render live transcripts (assistant text appearing token-by-token,
tool-use indicators, cost-so-far).

## Why now

Without this, the W4-04 3×3 grid would just be 9 status pills
that flip Idle/Running. The owner directive 2026-05-07 §1B
("ajanların neler yaptığını nasıl bir süreç izlediklerini de canlı
olarak görüntülemiş olurum") explicitly calls for live process
visibility. W4-03 is that substrate.

## Charter alignment

No tech-stack change. New Tauri event channel + Specta-typed
payload + frontend listener hook. The event-channel pattern
already exists in W3-12c (`swarm:job:{id}:event`); W4-03 adds a
per-agent channel using the same convention.

## Scope

### 1. New `SwarmAgentEvent` enum (`agent_registry.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwarmAgentEvent {
    /// `PersistentSession::spawn` succeeded; the registry slot just
    /// flipped from `NotSpawned` → `Idle`.
    Spawned { profile_id: String },
    /// `acquire_and_invoke_turn` is about to write a user message.
    /// `turn_index` mirrors the registry's `turns_taken` BEFORE the
    /// new turn (so the first turn is `turn_index: 0`).
    TurnStarted { turn_index: u32 },
    /// One streaming text delta from claude's stdout. May fire many
    /// times per turn. Frontend appends to a per-turn buffer.
    AssistantText { delta: String },
    /// Claude is using a tool. `name` is the tool name (Read, Edit,
    /// Glob, etc.); `input_summary` is a one-line truncation of the
    /// tool's input (e.g. `path: app/src/components/SwarmJobList.tsx`)
    /// so the UI can show "Scout is reading SwarmJobList.tsx" without
    /// exposing the full tool input JSON.
    ToolUse { name: String, input_summary: String },
    /// Turn finished cleanly. Final assistant text + accounting.
    Result {
        assistant_text: String,
        total_cost_usd: f64,
        turn_count: u32,
    },
    /// Specialist emitted a `neuron_help` JSON block (W4-05). W4-03
    /// reserves the variant so W4-05 can ship without widening the
    /// Specta-emitted type.
    HelpRequest { reason: String, question: String },
    /// Turn ended (success or cancel — not crash); slot is back to
    /// `Idle`.
    Idle,
    /// Session crashed unrecoverably. Slot is `Crashed`; next
    /// `acquire` will respawn.
    Crashed { error: String },
}
```

Same `tag = "kind"` shape as `SwarmJobEvent` so the frontend
listener pattern is uniform.

### 2. New `StreamEvent::ToolUse` variant (`transport.rs`)

The existing `classify_event` returns `StreamEvent::Other` for
`tool_use` content blocks. W4-03 adds a new variant + parser:

```rust
pub(crate) enum StreamEvent {
    SystemInit { session_id: String },
    AssistantDelta { text: String },
    /// New: claude is using a tool.
    ToolUse { name: String, input_summary: String },
    ResultSuccess { ... },
    ResultError { ... },
    Other,
}
```

`classify_event` walks `message.content` and emits `ToolUse` for
each `tool_use` block, joining the stringified input keys into a
short summary (capped ~120 chars to prevent log spam from huge
tool inputs).

The existing `AssistantDelta` path is unchanged; the two variants
fire independently per content block.

### 3. `PersistentSession::invoke_turn` event sink

```rust
pub async fn invoke_turn(
    &mut self,
    user_message: &str,
    timeout: Duration,
    cancel: Arc<Notify>,
    event_sink: Option<UnboundedSender<SwarmAgentEvent>>,
) -> Result<InvokeResult, AppError>;
```

When `event_sink` is `Some`:
- For each `StreamEvent::AssistantDelta { text }` classified inside
  `read_until_result`, send `SwarmAgentEvent::AssistantText { delta: text }`
- For each `StreamEvent::ToolUse { name, input_summary }`, send
  `SwarmAgentEvent::ToolUse { name, input_summary }`

When `event_sink` is `None`, behavior matches W4-01 verbatim — the
unit tests for one-shot transport / orchestrator decide stay green
without changes (they call `invoke_turn` with `None`).

The `mpsc::UnboundedSender` is the standard tokio channel; the
registry creates the receiver side and forwards to Tauri.

### 4. Registry emit hooks

`SwarmAgentRegistry::acquire_and_invoke_turn` becomes the central
emit point:

1. After `PersistentSession::spawn` succeeds:
   - `app.emit(channel_for(ws, agent), SwarmAgentEvent::Spawned { profile_id })`
2. Before calling `session.invoke_turn`:
   - `app.emit(..., SwarmAgentEvent::TurnStarted { turn_index: slot.turns_taken })`
3. Create an `mpsc::unbounded_channel()`. Spawn a forwarder task
   that consumes from the receiver and emits on the per-agent
   channel until the sender closes.
4. Pass the sender to `invoke_turn`.
5. After `invoke_turn` returns:
   - On `Ok`: `app.emit(..., Result { ... })` then `app.emit(..., Idle)`
   - On `Err(Cancelled)`: `app.emit(..., Idle)` (no Crashed —
     cancel is graceful)
   - On `Err(other)`: `app.emit(..., Crashed { error: ... })`
6. Drop the sender so the forwarder task exits.

Channel naming: `swarm:agent:{workspace_id}:{agent_id}:event`.

### 5. Frontend `useAgentEvents` hook

```typescript
// app/src/hooks/useAgentEvents.ts

import { useEffect, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { SwarmAgentEvent } from '../lib/bindings';

const TAIL_CAP = 200;

export function useAgentEvents(
  workspaceId: string,
  agentId: string,
): SwarmAgentEvent[] {
  const [events, setEvents] = useState<SwarmAgentEvent[]>([]);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    const channel = `swarm:agent:${workspaceId}:${agentId}:event`;
    listen<SwarmAgentEvent>(channel, (event) => {
      setEvents((prev) => {
        const next = [...prev, event.payload];
        return next.length > TAIL_CAP ? next.slice(-TAIL_CAP) : next;
      });
    })
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch((err) => {
        console.warn('[useAgentEvents] listen failed', channel, err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [workspaceId, agentId]);

  return events;
}
```

Same pattern as `useSwarmJob`'s event subscription (W3-12c). Ring
buffer caps at 200 events per agent so a long session can't OOM
the renderer.

### 6. Specta event type registration

In `lib.rs::specta_builder_for_export`, register `SwarmAgentEvent`
explicitly (events are a side channel, Specta only walks types
reachable from registered commands):

```rust
.typ::<crate::swarm::SwarmAgentEvent>()
```

bindings.ts gets the `SwarmAgentEvent` discriminated union next to
`SwarmJobEvent`.

### 7. Tests

#### Rust unit tests (~10):

- `swarm_agent_event_serializes_with_kind_tag` — round-trip
  every variant, assert wire shape uses `kind: "spawned"` etc.
- `classify_event_parses_tool_use_block` — JSON fixture with a
  `tool_use` content block; asserts `StreamEvent::ToolUse` with
  expected name + input summary
- `classify_event_truncates_long_tool_input_to_120_chars` —
  fixture with a 500-char input; assert summary is ≤ 120 + "…"
- `invoke_turn_with_event_sink_forwards_assistant_text` — drive
  a turn with a fixture stream-json containing 3 assistant
  deltas; assert the receiver gets 3 AssistantText events
- `invoke_turn_with_event_sink_forwards_tool_use` — fixture
  with a tool_use block; assert ToolUse event lands
- `invoke_turn_with_none_sink_emits_no_events` — pass `None`,
  drive a turn, assert no observable event-channel side effect
  (regression guard for the W4-01 pre-event behaviour)
- `registry_acquire_emits_spawned_then_turn_started_then_result_then_idle` —
  hook a mock event consumer onto the per-agent channel,
  drive a happy-path turn, assert event sequence
- `registry_acquire_emits_crashed_on_invoke_error` —
  drive a failed turn, assert `Crashed` event with the error
  message
- `registry_acquire_emits_idle_not_crashed_on_cancelled` —
  drive a cancelled turn, assert `Idle` event lands instead
  of `Crashed`

#### Frontend tests (~3):

- `useAgentEvents collects events from the channel` —
  mock `@tauri-apps/api/event::listen`, fire 3 events, assert
  hook returns array of length 3 in order
- `useAgentEvents caps the buffer at 200` — fire 250 events,
  assert returned array is length 200 (the most recent 200)
- `useAgentEvents resubscribes when (workspaceId, agentId) changes` —
  mount with (ws-a, scout); change to (ws-b, scout); assert
  unlisten was called for the old channel and the new channel
  is now live

## Files touched

- modified: `src-tauri/src/swarm/agent_registry.rs` — add
  `SwarmAgentEvent` enum, registry emit hooks, channel-name helper
- modified: `src-tauri/src/swarm/persistent_session.rs` — add
  `event_sink` parameter to `invoke_turn`; thread through to the
  read loop; emit AssistantText + ToolUse from inside
- modified: `src-tauri/src/swarm/transport.rs` — add
  `StreamEvent::ToolUse` + parser update; expose `StreamEvent` as
  `pub(crate)` if not already
- modified: `src-tauri/src/swarm/mod.rs` — re-export `SwarmAgentEvent`
- modified: `src-tauri/src/lib.rs` — register
  `crate::swarm::SwarmAgentEvent` on the specta builder
- new: `app/src/hooks/useAgentEvents.ts`
- new: `app/src/hooks/useAgentEvents.test.tsx`
- regenerate: `app/src/lib/bindings.ts`

Approximately 500-700 LoC including tests. S/M-sized.

## Acceptance gates

1. `cd src-tauri && cargo build --lib` → exit 0
2. `cd src-tauri && cargo test --lib` → green; new test count
   delta ≥ 9 (registry + persistent_session + transport)
3. `cd src-tauri && cargo check --all-targets` → exit 0
4. `pnpm gen:bindings:check` → exit 0 post-commit
5. `pnpm typecheck` → exit 0
6. `pnpm lint` → exit 0
7. `pnpm test --run` → green; +3 frontend tests
8. Manual smoke (post-merge, optional): start `pnpm tauri dev`,
   trigger any swarm job, watch the event log via
   `window.__TAURI__.event.listen('swarm:agent:default:scout:event', console.log)`
   in DevTools — confirm Spawned → TurnStarted → AssistantText
   bursts → Result → Idle stream lands.

## Out of scope (W4-03)

- ❌ The 3×3 grid UI itself — W4-04
- ❌ AgentPane component / structured transcript renderer — W4-04
- ❌ neuron_help parsing + Coordinator routing — W4-05 (the
  `HelpRequest` event variant is reserved here so W4-05 doesn't
  widen the type)
- ❌ FSM persistent-transport adapter — W4-06 (the FSM still
  uses one-shot `SubprocessTransport`; only the Orchestrator
  decide path exercises events in W4-03)
- ❌ Per-event persistence to SQLite — events are in-memory only,
  consumed by the live UI
- ❌ Replay of past events on remount — the W4-04 grid binds
  `useAgentEvents` on mount and only sees events from that point
  forward; a future "show me the last 100 events" surface can be
  layered on top via a `swarm:agent:tail` IPC if needed

## Notes / risks

- **Channel proliferation**: 9 agents × N workspaces ×
  per-event-emit. With single-workspace UX (default) we have at
  most 9 channels per app run. Tauri's emit cost is low; not a
  concern.
- **AssistantText event volume**: a single turn can produce
  100-500 deltas. Per-event `app.emit` Tauri overhead is ~tens of
  microseconds; total per turn ~5-50 ms — well within budget.
- **Forwarder task lifecycle**: spawned per-acquire. If
  `invoke_turn` panics, the sender drops, the forwarder exits.
  No leak.
- **`event_sink: None` path**: the orchestrator decide IPC
  (W3-12k1) doesn't go through the registry; it constructs a
  one-shot `SubprocessTransport`. So orchestrator decide doesn't
  emit per-agent events in W4-03. That's fine — the UI's chat
  panel already has its own loading indicator. A future WP could
  migrate orchestrator decide to the registry too.
- **ToolUse input summary**: capped at 120 chars to prevent log
  spam. The full input lives in the assistant_text on the Result
  event; the ToolUse event is just a live-feed indicator.
- **Charter §"Hard constraints" #4 (OKLCH only)**: doesn't apply
  to W4-03 backend; the frontend hook is non-visual.

## Sub-agent reminders

- Do NOT touch FSM code. W4-06 is the FSM cutover.
- Do NOT add `neuron_help` parsing — W4-05.
- After editing `transport.rs::classify_event`, the existing
  `stream_json_line_parser` test in `transport::tests` may need
  a new fixture for the ToolUse path. Check the existing test
  before adding new ones; reuse fixtures where possible.
- Reuse the W3-12c event-channel pattern verbatim (channel name,
  payload type, emit pattern). Don't reinvent.
- Update bindings before declaring done:
  `pnpm gen:bindings && pnpm gen:bindings:check`.
- Final commit: `feat: WP-W4-03 per-agent event channel +
  streaming AssistantText/ToolUse`. Co-Authored-By trailer.
