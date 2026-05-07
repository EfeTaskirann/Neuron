---
id: WP-W4-01
title: PersistentSession transport (multi-turn `claude` subprocess, alongside one-shot SubprocessTransport)
owner: TBD
status: not-started
depends-on: []
acceptance-gate: "New `PersistentSession` struct in `src-tauri/src/swarm/persistent_session.rs` that holds a long-lived `claude` child process and supports `invoke_turn(user_message, timeout, cancel)` for multi-message round-trips against the same session. Existing `SubprocessTransport` is preserved (one-shot use cases keep working). Cancel-current-turn does NOT kill the child. `cargo test --lib` green; one new `#[ignore]`'d real-claude integration smoke that drives a two-turn session where turn 2 references turn 1's content."
---

## Goal

Add a sibling transport to W3-11's `SubprocessTransport` that keeps
the `claude` child process alive across multiple invocations of the
same session. This is the foundational layer for W4 — every other
W4 sub-WP (registry, event channel, agent panes, Coordinator hub,
FSM adapter) builds on a per-session multi-turn primitive.

## Why now

Owner directive 2026-05-07 §1B: agent sessions live as long as the
workspace. Today's one-shot subprocess pattern can't carry context
across stages — every stage cold-starts (~30-60s on Windows AV
first-spawn). Persistent sessions remove that overhead AND give
each agent a stable identity that the user can watch in real time.

## Charter alignment

No tech-stack change. Reuses the existing `claude` CLI invocation
contract (`-p --input-format stream-json --output-format stream-json
--append-system-prompt-file --max-turns ...` from W3-11
`binding::build_specialist_args`). The OAuth subscription env
(`subscription_env`) is preserved. Same persona-tmp-file mechanic.
The change is purely in the lifecycle: child outlives the call.

## Scope

### 1. New module `src-tauri/src/swarm/persistent_session.rs`

```rust
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Runtime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{Mutex, Notify};

use crate::error::AppError;
use crate::swarm::profile::Profile;
use crate::swarm::transport::{InvokeResult, RingBuffer, STDERR_RING_CAPACITY};

/// A single long-lived `claude` child wired up to drive multi-turn
/// stream-json conversations. Spawned once per (workspace, agent)
/// pair by the W4-02 registry; dropped (which kills the child via
/// `kill_on_drop`) on workspace close.
///
/// Thread-safety contract: not `Sync`. The W4-02 registry serialises
/// access per agent — at most one `invoke_turn` is in flight per
/// session at a time. Concurrent turns against the same session are
/// a programming error and will deadlock on the stdin write.
pub struct PersistentSession {
    profile_id: String,
    persona_tmp_path: PathBuf,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr_ring: Arc<Mutex<RingBuffer>>,
    turns_taken: u32,
}

impl PersistentSession {
    /// Spawn a fresh `claude` child against `profile`. Uses the
    /// same arg builder + env strip as `SubprocessTransport`, but
    /// retains the child handle and pipes for multi-turn use.
    pub async fn spawn<R: Runtime>(
        app: &AppHandle<R>,
        profile: &Profile,
    ) -> Result<Self, AppError>;

    /// Send `user_message` as the next turn, await the next `result`
    /// event, return the parsed `InvokeResult`. Child stays alive on
    /// return.
    ///
    /// `cancel` is observed via `tokio::select!` against the read
    /// loop. Cancel signals truncate the in-flight turn (returns
    /// `AppError::Cancelled`); the child is NOT killed — the next
    /// `invoke_turn` against the same session is well-defined and
    /// reads any leftover bytes flushed by claude before the cancel
    /// took effect. Specifically: cancel discards bytes until the
    /// next `result` event boundary is observed (best-effort drain;
    /// up to a small budget), then returns control. If claude has
    /// already emitted a `result` and is awaiting the next user
    /// message, drain is a no-op.
    pub async fn invoke_turn(
        &mut self,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> Result<InvokeResult, AppError>;

    /// Stage diagnostics: how many turns has this session taken?
    /// Read by the W4-02 registry to decide when to respawn under
    /// the turn-cap policy.
    pub fn turns_taken(&self) -> u32 { self.turns_taken }

    pub fn profile_id(&self) -> &str { &self.profile_id }

    /// Explicit shutdown. Sends `claude`'s stdin EOF (drops the
    /// pipe), waits up to 2s for graceful exit, then kills the
    /// child. Removes the persona tmp file.
    pub async fn shutdown(self) -> Result<(), AppError>;
}

impl Drop for PersistentSession {
    /// Best-effort cleanup if the caller forgot `shutdown()`.
    /// `kill_on_drop(true)` was set at spawn time, so the child
    /// will be terminated; we just remove the persona tmp file
    /// here.
    fn drop(&mut self) { /* unlink persona tmp; ignore errors */ }
}
```

### 2. Read-loop multi-turn semantics

The current one-shot `SubprocessTransport::invoke` (transport.rs)
reads stream-json events until `result.subtype == "success"` (or an
error variant), then drops the child. For multi-turn we keep the
same parser but treat `result` as the **end of one turn**, not the
end of the session. The next `invoke_turn` call starts a fresh
read scope that runs until the *next* `result`.

Implementation: extract the existing event-reader inner loop from
`transport.rs::SubprocessTransport::invoke` into a free fn:

```rust
async fn read_one_turn<R: tokio::io::AsyncBufReadExt + Unpin>(
    reader: &mut R,
    stderr_ring: Arc<Mutex<RingBuffer>>,
    timeout: Duration,
    cancel: Arc<Notify>,
) -> Result<InvokeResult, AppError>;
```

Both `SubprocessTransport::invoke` (drops child after) and
`PersistentSession::invoke_turn` (preserves child after) call this
helper. No behavioral change to the existing one-shot path.

### 3. Stdin user-message framing

Stream-json input format expects one JSON object per line. The
existing one-shot path writes a single `{"type":"user", ...}` line
then closes stdin (signals "no more turns"). For multi-turn we
write the same line but DON'T close stdin — claude waits for the
next message after the `result` event. The next `invoke_turn` call
writes another `{"type":"user", ...}` line.

The exact JSON shape is the same as the W3-11 contract; reuse the
existing `binding::format_user_message` helper. No new wire format.

### 4. Cancel semantics

Per W3-12c there's a `tokio::sync::Notify` + `notify_waiters()`
pattern for FSM cancel. Same `Notify` style here, but:
- Receiving cancel during `read_one_turn`: stop reading, drain
  pipe up to ~1s budget (so claude's already-emitted bytes don't
  poison the next turn), return `AppError::Cancelled`. Child
  stays alive.
- Receiving cancel during a `shutdown`: `shutdown()` is
  uninterruptible — we always want to reach the kill state.

The drain-up-to-budget strategy is a defense against a benign
race where claude finishes a turn just as cancel fires — we'd
rather return `Cancelled` than partial data, and we want the
session to be ready for the next turn.

### 5. Extract `RingBuffer` + `STDERR_RING_CAPACITY` to `pub` on `transport.rs`

Currently private. The persistent module needs them; expose via
`pub(crate)` on the transport module. No behavioral change.

### 6. Tests (12+ new)

#### Mock-stream tests (`persistent_session::tests`)

Adapt the test pattern from `transport::tests`. Use a duplex
pipe to fake `claude`'s stdin/stdout:

- `single_turn_round_trip` — write user message, fake assistant
  emits `result`, assert `InvokeResult` matches.
- `two_turn_round_trip` — drive turn 1, then turn 2 against the
  same fake; assert both `InvokeResult` shapes are correct AND
  the session is still alive (`child.try_wait()` returns `None`).
- `twenty_turn_session` — stress test, 20 sequential turns; assert
  no leak, `turns_taken()` reads correctly.
- `cancel_mid_turn_returns_cancelled_and_session_alive` — write
  user message, signal cancel before fake emits `result`, assert
  `Err(Cancelled)` and the session can still drive the NEXT turn.
- `cancel_after_result_is_noop` — cancel signals after the result
  event has been read; the next `invoke_turn` works without
  losing data.
- `timeout_per_turn_does_not_kill_session` — timeout fires before
  fake emits `result`; turn returns `Err(Timeout)` but session
  stays alive for retry.
- `shutdown_kills_child` — explicit `shutdown()`; assert the
  `try_wait()` returns `Some(_)` and the persona tmp file is
  unlinked.
- `drop_kills_child_via_kill_on_drop` — drop the session without
  shutdown; `kill_on_drop(true)` should reap the child.
- `error_max_turns_event_returns_swarminvoke` — fake emits
  `result.subtype="error_max_turns"`; turn returns `Err(SwarmInvoke)`
  with the canonical message.
- `multi_turn_after_error_max_turns_continues` — claude reports
  max_turns on turn 5 (per-turn budget exhausted by claude's own
  internal loop, separate from session-level cap); turn returns
  Err but session is alive and turn 6 succeeds.

#### Profile-aware test

- `spawn_uses_profile_max_turns_and_allowed_tools` — assert the
  argv passed to the fake binary matches the W3-11 contract
  (`-p`, `--input-format stream-json`, `--max-turns N`,
  `--allowedTools "..."`, `--append-system-prompt-file <tmp>`).
  Same shape as the existing W3-11 unit test for one-shot.

#### Real-claude integration smoke (`#[ignore]`'d)

- `integration_persistent_two_turn_real_claude` — spawn a session
  against the `scout` profile, ask "Find the `formatRelativeMs`
  function in `app/src/components/SwarmJobList.tsx`. Reply only
  with `FOUND` or `NOT FOUND` — nothing else.", then turn 2:
  "Now tell me the file path you searched in your previous reply."
  Assert turn 2's `assistant_text` contains the file path
  string — proves session context carried.

### 7. Module wiring

- `swarm/mod.rs`: re-export `PersistentSession` so external call
  sites (W4-02 registry, eventually) can import it without a
  deep path.
- `lib.rs`: no changes (PersistentSession is not directly an IPC
  surface in W4-01).

## Files touched

- new: `src-tauri/src/swarm/persistent_session.rs`
- new: `src-tauri/src/swarm/persistent_session_tests.rs` (or inline
  `#[cfg(test)] mod tests` — owner picks at authoring; if the test
  file would push `persistent_session.rs` past ~600 lines, split)
- modified: `src-tauri/src/swarm/transport.rs` — extract
  `read_one_turn`, expose `RingBuffer` + `STDERR_RING_CAPACITY`
  as `pub(crate)`; no behavioral change to one-shot path
- modified: `src-tauri/src/swarm/mod.rs` — `pub use
  persistent_session::PersistentSession;`
- modified: nothing else — no IPC, no DB, no frontend in W4-01

Approximately 600-900 LoC including tests. M-sized.

## Acceptance gates (sub-agent must run; orchestrator re-verifies post-return)

1. `cd src-tauri && cargo build --lib` → exit 0
2. `cd src-tauri && cargo test --lib` → green; new test count
   delta is ≥ 11 from the W4-01 module, no flakes
3. `cd src-tauri && cargo check --all-targets` → exit 0
4. `pnpm gen:bindings:check` → exit 0 (W4-01 adds no new IPC, so
   bindings are unchanged; check guards against accidental drift)
5. Existing W3 real-claude smokes still pass — sub-agent runs
   `integration_research_only_real_claude` as a regression check
   to confirm one-shot transport untouched
6. New W4 smoke `integration_persistent_two_turn_real_claude`
   passes with the test goal above (`#[ignore]` flagged so CI
   stays green; orchestrator runs manually post-merge)

## Out of scope (W4-01)

- ❌ Agent registry (W4-02 owns lifecycle, lazy spawn, status,
  workspace lock)
- ❌ Tauri event channel for per-agent events (W4-03)
- ❌ FSM integration — `CoordinatorFsm::run_job` still uses
  one-shot `SubprocessTransport` after this WP. W4-06 wires it.
- ❌ UI changes — no frontend touched
- ❌ `neuron_help` parser — W4-05 owns the help-request contract
- ❌ Turn-cap respawn policy — W4-02 owns the registry; W4-01
  exposes `turns_taken()` so the registry can read it but does
  NOT enforce a cap
- ❌ Specta event types — W4-03 registers them
- ❌ Replacing `SubprocessTransport` — kept alongside; the
  `commands::swarm::swarm_test_invoke` and
  `commands::swarm::swarm_orchestrator_decide` IPCs continue to
  use one-shot

## Notes / risks

- **Stdin EOF handling on shutdown**: dropping `ChildStdin` should
  signal EOF to claude, which then exits cleanly. If `claude` is
  mid-tool-use it may take seconds to wind down. The 2s graceful
  budget in `shutdown()` is generous; on timeout we kill. Test
  the timeout path explicitly.
- **Stderr drain**: same ring-buffer pattern as one-shot
  (`STDERR_RING_CAPACITY` bytes, oldest-first eviction).
  Stderr-drain task spawned in `spawn()`, joined in `shutdown()`.
- **Persona tmp file lifecycle**: written at spawn, unlinked at
  shutdown OR drop. Test the leak case (drop without shutdown).
- **Cancel during stdin write**: a long user-message on a slow
  pipe could be mid-write when cancel fires. The write itself is
  not cancelled (it's small — single line); we only cancel the
  read scope. Document this so the registry doesn't expect
  perfect cancel symmetry.
- **kill_on_drop interaction with async runtime shutdown**: when
  the Tauri app exits (window close), the tokio runtime drops all
  remaining tasks. `kill_on_drop` reaps the child via the runtime's
  cleanup. Test with a controlled drop in the unit suite.
- **Per-turn timeout vs session-level**: each turn has its own
  timeout (passed in by the caller, default 180s same as W3-12).
  There's no session-level timeout in W4-01 — sessions live until
  workspace close OR registry-driven respawn (W4-02 turn-cap).

## Sub-agent reminders

- Do NOT touch FSM code (`src-tauri/src/swarm/coordinator/`). FSM
  integration is W4-06.
- Do NOT add Tauri event emit calls. Event channel is W4-03.
- Do NOT delete `SubprocessTransport`. Sibling, not replacement.
- Test mocks should reuse `transport.rs::tests`'s duplex-pipe
  pattern verbatim — copy + adapt, don't reinvent.
- After editing any `.md` under `src-tauri/src/swarm/agents/`,
  touch `src-tauri/src/swarm/profile.rs` to invalidate the
  `include_dir!` cache. (W4-01 is unlikely to touch personas; if
  the smoke goal requires a persona tweak, follow the touch
  protocol.)
- Subscription OAuth env-strip is mandatory (`subscription_env`).
  Don't introduce ANTHROPIC_API_KEY anywhere.
- Charter §"Hard constraints" #4 (OKLCH only) doesn't apply —
  W4-01 is backend-only — but cross-cutting Rust style: 80-col
  comments, `///` doc on every pub item, snake_case everywhere.
- Final commit message: `feat: WP-W4-01 PersistentSession transport
  (multi-turn claude subprocess + tests)`. Add Co-Authored-By
  trailer.
