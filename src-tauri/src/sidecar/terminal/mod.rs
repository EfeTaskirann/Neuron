//! WP-W2-06 — Terminal PTY supervisor.
//!
//! Owns the lifecycle of every shell process spawned via `terminal:spawn`
//! and proxies stdin/stdout between the shell's PTY and the frontend.
//!
//! Wiring at a glance:
//!
//! ```text
//! lib.rs::run().setup(...)
//!     ├── db::init                          → SqlitePool managed in app state
//!     └── sidecar::terminal::TerminalRegistry::new
//!             → empty registry, kept in app state
//!
//! commands/terminal.rs::terminal_spawn
//!     └── TerminalRegistry::spawn_pane(opts, app, pool)
//!             → portable_pty::native_pty_system().openpty(...)
//!             → slave.spawn_command(shell)
//!             → INSERT panes(..., status='running', pid)
//!             → spawn_blocking reader  → emits panes:{id}:line events
//!                                       → tracks awaiting_approval state
//!             → spawn_blocking waiter  → updates panes.status on exit
//!             → returns Pane row from DB
//!
//! lib.rs::run().run(|_, ExitRequested| ...)
//!     └── TerminalRegistry::shutdown_all
//!             → kill every alive child, no orphan shells
//! ```
//!
//! Output framing differs from `agent.rs`. The Python sidecar uses
//! length-prefixed JSON frames (`framing.rs`). PTY data is **raw bytes**:
//! the reader task buffers bytes, splits on `\n`, emits one event per
//! complete line, and stores a CSI-stripped copy in the in-memory ring
//! buffer (5,000 lines per pane; oldest 1,000 dropped on overflow). On
//! pane close the ring buffer is flushed to `pane_lines` so the UI can
//! re-hydrate the scrollback after restart via `terminal:lines`.
//!
//! ## Module layout
//!
//! The supervisor is split into focused submodules; this file owns the
//! registry, the per-pane state, and the public command surface:
//!
//! - [`text`] — pure byte/text helpers (CSI strip, line-end trim, DSR-CPR).
//! - [`command`] — shell/command resolution + the claude env-scrub list.
//! - [`approval`] — awaiting-approval regex sets + banner extraction.
//! - [`reader`] — the reader + waiter background tasks and status flips.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Runtime};
use tokio::sync::Mutex as AsyncMutex;
use ulid::Ulid;

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{Pane, PaneSpawnInput};
use crate::tuning::{KILL_GRACE, RING_BUFFER_CAP};

mod approval;
mod command;
mod reader;
mod text;
#[cfg(test)]
mod tests;

use command::{
    default_shell, expand_cwd, infer_agent_kind, tokenize_command, CLAUDE_AGENT_STRIPPED_ENV,
};
use reader::{flush_ring_to_db, run_reader, run_waiter};

// Tunables (RING_BUFFER_CAP, RING_BUFFER_DROP, APPROVAL_WINDOW_LINES,
// READ_CHUNK_BYTES, KILL_GRACE, WAIT_POLL, MAX_PENDING_BYTES) live in
// `crate::tuning` so the runtime profile is editable in one place.

// --------------------------------------------------------------------- //
// Pane status discriminants                                             //
// --------------------------------------------------------------------- //

/// Values written to the `panes.status` column. The strings here are
/// the canonical state-machine identifiers consumed by the frontend
/// (NEURON_TERMINAL_REPORT § state machine):
///
///   `idle` (WP-W2-03 stub) → `starting` (process spawning) →
///   `running` → `awaiting_approval` (regex match) →
///   `running` → `success` | `error`.
mod status {
    pub const STARTING: &str = "starting";
    pub const RUNNING: &str = "running";
    pub const AWAITING: &str = "awaiting_approval";
    pub const SUCCESS: &str = "success";
    pub const ERROR: &str = "error";
}

// --------------------------------------------------------------------- //
// Per-pane in-memory state                                              //
// --------------------------------------------------------------------- //

/// One ring-buffer entry. `seq` is the per-pane monotonic counter
/// emitted to the frontend so consumers can deduplicate or scroll back
/// without ambiguity. `text` carries the CSI-stripped line text;
/// the live event payload preserves the raw bytes for xterm.js
/// (WP-W2-08).
#[derive(Debug, Clone)]
struct RingLine {
    seq: i64,
    /// `'sys'|'prompt'|'command'|'thinking'|'tool'|'out'|'err'`.
    /// Persisted to `pane_lines.k`. PTY output uses `out`; system
    /// notices (e.g. `exit 0`) use `sys`.
    kind: &'static str,
    text: String,
}

/// State carried by each pane while alive. Stored under a `Mutex` so
/// commands can mutate the writer (`terminal:write`) and the master
/// (`terminal:resize`) without coordinating with the reader and waiter
/// tasks (which each own a clone).
struct PaneState {
    /// Sequence number to stamp on the next emitted line. Monotonic
    /// from 1 within one process lifetime; not contiguous across app
    /// restarts (the DB-backed scrollback restarts at 1).
    next_seq: i64,
    /// In-memory ring of recent lines. Flushed to `pane_lines` on pane
    /// close.
    ring: VecDeque<RingLine>,
    /// PTY master — what we resize. The reader task holds a separate
    /// reader clone obtained via `try_clone_reader()`. `Option` so the
    /// waiter can `take()` and drop it on child exit (essential on
    /// Windows ConPTY for the reader's blocking `read()` to unblock).
    master: Option<Box<dyn MasterPty + Send>>,
    /// Stdin writer — `take_writer()` is one-shot, so `terminal:write`
    /// must serialize through this slot.
    writer: Option<Box<dyn Write + Send>>,
    /// Independent kill handle. Cloned from the child once and held
    /// here so `kill_pane` can fire even if the waiter task is
    /// currently blocked in `try_wait()`.
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Latest known status. Mirrors `panes.status` so commands can
    /// answer "is this pane alive?" without a DB hit.
    status: &'static str,
    /// Captured PID for diagnostic logging. Persisted to `panes.pid`
    /// at INSERT time and surfaced to the frontend via `Pane.pid`. Held
    /// here in case future debug commands want to query the live PID
    /// without touching SQLite.
    #[allow(dead_code)]
    pid: Option<u32>,
}

/// Top-level registry. One per app process. Cloned cheaply because the
/// inner state is `Arc`-backed.
#[derive(Clone)]
pub struct TerminalRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    /// Map keyed by `Pane.id`. The outer `AsyncMutex` is held only
    /// briefly (lookup or insert); per-pane state mutation happens
    /// under the inner `Mutex` to avoid awaiting across blocking
    /// PTY writes / resizes.
    panes: AsyncMutex<HashMap<String, Arc<Mutex<PaneState>>>>,
}

// --------------------------------------------------------------------- //
// Public API — what the Tauri commands call into                        //
// --------------------------------------------------------------------- //

impl TerminalRegistry {
    /// Build an empty registry for `app.manage(...)`.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                panes: AsyncMutex::new(HashMap::new()),
            }),
        }
    }

    /// Fork a PTY, spawn the platform default shell (or `opts.cmd` if
    /// supplied), insert a row in `panes`, and start the reader/waiter
    /// background tasks. Returns the freshly-inserted `Pane` row.
    pub async fn spawn_pane<R: Runtime>(
        &self,
        opts: PaneSpawnInput,
        app: AppHandle<R>,
        pool: DbPool,
    ) -> Result<Pane, AppError> {
        if opts.cwd.trim().is_empty() {
            return Err(AppError::InvalidInput("cwd must not be empty".into()));
        }

        // Resolve the shell command. Empty string is treated as "use
        // the platform default" — that matches the WP-W2-06 manual
        // smoke step (`cwd: '~'`, no `cmd`).
        let cmd = match opts.cmd.as_deref() {
            None => default_shell(),
            Some(c) if c.trim().is_empty() => default_shell(),
            Some(c) => c.to_string(),
        };
        let agent_kind = opts
            .agent_kind
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| infer_agent_kind(&cmd).into());

        let cols = opts.cols.unwrap_or(80).max(1);
        let rows = opts.rows.unwrap_or(24).max(1);

        // Resolve the cwd. `~` is expanded against the user's home
        // directory; on platforms where home_dir() is unavailable we
        // fall back to the literal `~` and let the shell deal with it.
        let cwd = expand_cwd(&opts.cwd);

        // Spawn the PTY pair. `pixel_width`/`pixel_height` are 0 here
        // because xterm.js does not currently care; WP-W2-08 will pass
        // measured cell dimensions when font metrics are available.
        let pty_pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::Internal(format!("openpty: {e}")))?;

        // Build the command. `cmd` may be just the program (e.g.
        // `pwsh.exe`) or a whole command line with arguments
        // (`/bin/sh -c "echo hello"`). Tokenise so `CommandBuilder`
        // gets the program and argv split correctly — without this,
        // portable-pty tries to spawn `"\"cmd.exe /c echo hello\""`
        // as a literal program name and fails with "path not found".
        let argv = tokenize_command(&cmd);
        if argv.is_empty() {
            return Err(AppError::InvalidInput("cmd is empty".into()));
        }
        // `set_controlling_tty(true)` is required on Unix so the shell
        // sees the PTY as its controlling terminal; otherwise job
        // control (Ctrl-C, Ctrl-Z) silently no-ops. The setting is
        // harmless on Windows.
        let mut builder = CommandBuilder::new(&argv[0]);
        for arg in &argv[1..] {
            builder.arg(arg);
        }
        builder.cwd(&cwd);
        builder.set_controlling_tty(true);

        // When the agent we're spawning is `claude-code`, scrub the
        // `CLAUDE*` / `ANTHROPIC*` env-var pollution the parent
        // process accumulates. When the Neuron desktop is itself
        // launched from inside a `claude` shell (or from a terminal
        // whose npm wrapper exports these for the install path),
        // the spawned claude REPL sees `CLAUDECODE=1` /
        // `CLAUDE_CODE_SESSION_ID=…` / `CLAUDE_CODE_EXECPATH=…`
        // and decides it's a nested instance — and re-runs the
        // OAuth "Select login method" picker on every spawn,
        // despite `~/.claude/.credentials.json` being valid.
        // Stripping these isolates each pane's claude as if it
        // were the user's first `claude` invocation of the day,
        // which is exactly what we want.
        //
        // `ANTHROPIC_API_KEY` and the provider switches are
        // stripped on the same grounds as `swarm::binding::
        // subscription_env()` — they'd silently flip the spawn
        // off the user's Pro/Max OAuth channel onto BYOK billing.
        if agent_kind == "claude-code" {
            for var in CLAUDE_AGENT_STRIPPED_ENV {
                builder.env_remove(var);
            }
        }
        // Apply caller-supplied env overrides AFTER the scrub list,
        // so `extra_env` can re-set anything we just removed AND can
        // point the spawned process at a private HOME / USERPROFILE.
        // Used by `swarm-term::session` to give each claude REPL its
        // own `~/.claude.json` (preventing the 9-way concurrent-write
        // race that progressively truncated the user's shared config
        // in the 2026-05-12 23:46Z smoke).
        if let Some(extra) = &opts.extra_env {
            for (k, v) in extra {
                builder.env(k, v);
            }
        }
        let child: Box<dyn Child + Send + Sync> = pty_pair
            .slave
            .spawn_command(builder)
            .map_err(|e| AppError::Internal(format!("spawn_command({cmd}): {e}")))?;

        let pid = child.process_id();
        let pid_i64 = pid.map(|p| p as i64);

        // Reader handle is cloned upfront (the trait does not let us
        // re-clone once the task has started). Same story for the
        // killer handle.
        let reader = pty_pair
            .master
            .try_clone_reader()
            .map_err(|e| AppError::Internal(format!("try_clone_reader: {e}")))?;
        let writer = pty_pair
            .master
            .take_writer()
            .map_err(|e| AppError::Internal(format!("take_writer: {e}")))?;
        let killer = child.clone_killer();

        let pane_id = format!("p-{}", Ulid::new());
        let workspace = opts.workspace.clone().unwrap_or_else(|| "personal".into());
        let role = opts.role.clone();

        // INSERT first — the DB row is the source of truth for the
        // `Pane` we return. Status starts at `starting` so the frontend
        // can show the spawning pill; the waiter task transitions it
        // through `running` once the child stays alive past the first
        // poll cycle.
        // RETURNING projects four `NULL AS …` columns for the
        // mock-shape parity fields (`tokens_in/out/cost_usd/uptime`)
        // that Pane carries with `#[sqlx(default)]` — without these
        // explicit projections sqlx's FromRow bails out on
        // `ColumnNotFound`. `approval` is `#[sqlx(skip)]`, so it's
        // always defaulted to `None` here regardless of the row
        // (a fresh pane has no banner yet).
        let pane_row = sqlx::query_as::<_, Pane>(
            "INSERT INTO panes (id, workspace, agent_kind, role, cwd, status, pid) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             RETURNING id, workspace, agent_kind, role, cwd, status, pid, \
                       started_at, closed_at, \
                       NULL AS tokens_in, NULL AS tokens_out, \
                       NULL AS cost_usd, NULL AS uptime",
        )
        .bind(&pane_id)
        .bind(&workspace)
        .bind(&agent_kind)
        .bind(role.as_deref())
        .bind(&cwd.to_string_lossy().to_string())
        .bind(status::STARTING)
        .bind(pid_i64)
        .fetch_one(&pool)
        .await?;

        // Stash the runtime state behind a clonable handle so the
        // reader and waiter tasks can mutate it independently.
        let state = Arc::new(Mutex::new(PaneState {
            next_seq: 1,
            ring: VecDeque::with_capacity(RING_BUFFER_CAP),
            master: Some(pty_pair.master),
            writer: Some(writer),
            killer,
            status: status::STARTING,
            pid,
        }));

        {
            let mut panes = self.inner.panes.lock().await;
            panes.insert(pane_id.clone(), state.clone());
        }

        // Reader task: loop on the blocking PTY reader, emit
        // `panes:{id}:line` per `\n`, drive the awaiting_approval
        // detection. Marked `spawn_blocking` because `Read` is sync.
        let reader_state = state.clone();
        let reader_app = app.clone();
        let reader_pool = pool.clone();
        let reader_pane_id = pane_id.clone();
        let reader_agent = agent_kind.clone();
        tokio::task::spawn_blocking(move || {
            run_reader(
                reader,
                reader_state,
                reader_app,
                reader_pool,
                reader_pane_id,
                reader_agent,
            );
        });

        // Waiter task: poll `try_wait()` until the child exits, then
        // flip the status to `success`/`error` and flush the ring
        // buffer to `pane_lines`. Marked `spawn_blocking` because
        // `Child::try_wait` is sync.
        let waiter_state = state.clone();
        let waiter_app = app.clone();
        let waiter_pool = pool.clone();
        let waiter_pane_id = pane_id.clone();
        let waiter_registry = self.clone();
        tokio::task::spawn_blocking(move || {
            run_waiter(
                child,
                waiter_state,
                waiter_app,
                waiter_pool,
                waiter_pane_id,
                waiter_registry,
            );
        });

        Ok(pane_row)
    }

    /// Write raw bytes to the pane's PTY stdin. Used by the keyboard
    /// passthrough: the frontend collects keystrokes and ships them
    /// here. Bytes are written verbatim; the shell handles line
    /// editing.
    /// Read-only snapshot of a pane's lifecycle status.
    ///
    /// Returns one of the values from `status::*` (`starting`,
    /// `running`, `awaiting_approval`, `success`, `error`) or `None`
    /// if no such pane is registered.
    ///
    /// Used by `swarm_term::bridge::process_one`'s pane-status gate to
    /// skip writes to panes that are in `awaiting_approval` (the
    /// receiver is blocked on a safety prompt and would silently
    /// swallow the route) or `error` (the receiver is already dead —
    /// write would just log `Pane … has no stdin`). A `success` / idle
    /// pane IS a valid delivery target and is NOT gated.
    pub async fn pane_status(&self, pane_id: &str) -> Option<&'static str> {
        let panes = self.inner.panes.lock().await;
        let state = panes.get(pane_id).cloned()?;
        drop(panes);
        let guard = state.lock().ok()?;
        Some(guard.status)
    }

    pub async fn write_to_pane(&self, pane_id: &str, data: &[u8]) -> Result<(), AppError> {
        let panes = self.inner.panes.lock().await;
        let state = panes
            .get(pane_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("Pane {pane_id} not found")))?;
        drop(panes);

        // Off-thread the actual write because the writer is a blocking
        // `std::io::Write`. We hold the inner Mutex only inside
        // spawn_blocking so the IPC task is not stuck waiting on a
        // pipe.
        let data = data.to_vec();
        let pane_id_for_err = pane_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), AppError> {
            let mut guard = state
                .lock()
                .map_err(|e| AppError::Internal(format!("pane state poisoned: {e}")))?;
            let writer = guard.writer.as_mut().ok_or_else(|| {
                AppError::Conflict(format!("Pane {pane_id_for_err} has no stdin (closed)"))
            })?;
            writer
                .write_all(&data)
                .map_err(|e| AppError::Internal(format!("pty write: {e}")))?;
            writer
                .flush()
                .map_err(|e| AppError::Internal(format!("pty flush: {e}")))?;
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking joined err: {e}")))?
    }

    /// Resize the PTY (SIGWINCH equivalent). Throttling is the caller's
    /// job — WP-W2-06 § "Notes / risks" caps the frontend at ≤10/sec
    /// because Windows ConPTY is known to deadlock under high resize
    /// throughput. We only apply the resize here.
    pub async fn resize_pane(
        &self,
        pane_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), AppError> {
        if cols == 0 || rows == 0 {
            return Err(AppError::InvalidInput(
                "cols and rows must be > 0".into(),
            ));
        }
        let panes = self.inner.panes.lock().await;
        let state = panes
            .get(pane_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("Pane {pane_id} not found")))?;
        drop(panes);

        let pane_id_for_err = pane_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), AppError> {
            let guard = state
                .lock()
                .map_err(|e| AppError::Internal(format!("pane state poisoned: {e}")))?;
            let master = guard.master.as_ref().ok_or_else(|| {
                AppError::Conflict(format!("Pane {pane_id_for_err} is closed"))
            })?;
            master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| AppError::Internal(format!("pty resize: {e}")))?;
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking joined err: {e}")))?
    }

    /// Kill the pane's child process. The waiter task observes the
    /// exit and runs the post-mortem path (status flip, ring flush).
    /// The DB row's terminal state (`closed_at`, `status='success'`)
    /// is the waiter's job, not this method's; this only signals.
    pub async fn kill_pane(&self, pane_id: &str, pool: &DbPool) -> Result<(), AppError> {
        // First, signal the child (if still tracked in the registry).
        let state = {
            let panes = self.inner.panes.lock().await;
            panes.get(pane_id).cloned()
        };

        if let Some(state) = state {
            tokio::task::spawn_blocking(move || -> Result<(), AppError> {
                let mut guard = state
                    .lock()
                    .map_err(|e| AppError::Internal(format!("pane state poisoned: {e}")))?;
                // `ChildKiller::kill()` errors when the child has
                // already exited (Windows: ERROR_INVALID_PARAMETER /
                // os error 87; Unix: ESRCH). Both mean "the child is
                // not alive anymore", which is the same idempotent
                // outcome we want from `terminal:kill`. Swallow the
                // error in those cases — the waiter task has either
                // observed the exit already or will on its next poll.
                if let Err(e) = guard.killer.kill() {
                    let msg = e.to_string();
                    let already_dead = msg.contains("os error 87")
                        || msg.contains("os error 3") // ERROR_PATH_NOT_FOUND on stale handles
                        || msg.contains("ESRCH")
                        || msg.contains("No such process");
                    if !already_dead {
                        return Err(AppError::Internal(format!("kill: {e}")));
                    }
                }
                Ok(())
            })
            .await
            .map_err(|e| AppError::Internal(format!("spawn_blocking joined err: {e}")))??;

            // Best-effort — give the waiter a chance to observe the
            // exit and update the DB row before we return. We don't
            // gate on it; the waiter will eventually run.
            let _ = tokio::time::timeout(KILL_GRACE, async {
                loop {
                    let panes = self.inner.panes.lock().await;
                    if !panes.contains_key(pane_id) {
                        break;
                    }
                    drop(panes);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await;
            // Whether or not the waiter has cleaned up by now, the DB
            // row should be marked closed at the latest by this point;
            // if the waiter beat us to it, the UPDATE below is a no-op
            // because `closed_at IS NULL` excludes already-closed rows.
        } else {
            // No registry entry — either WP-W2-03 stub row never had a
            // PTY, or the pane was already cleaned up. Distinguish:
            let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM panes WHERE id = ?")
                .bind(pane_id)
                .fetch_optional(pool)
                .await?;
            if exists.is_none() {
                return Err(AppError::NotFound(format!("Pane {pane_id} not found")));
            }
        }

        // Fall-through DB update covers the WP-W2-03 idle stub case
        // (no registry entry, but a row exists in `panes`). Real PTY
        // panes have already been transitioned by the waiter task;
        // this UPDATE is bounded by `closed_at IS NULL` so it is a
        // no-op for them.
        let res = sqlx::query(
            "UPDATE panes SET status = 'closed', closed_at = strftime('%s','now') \
             WHERE id = ? AND closed_at IS NULL",
        )
        .bind(pane_id)
        .execute(pool)
        .await?;

        // If the waiter hadn't claimed the row yet AND the pane was
        // never registered, `rows_affected = 0` and we already errored
        // above. If it was registered, the waiter's UPDATE took
        // precedence — that's also fine.
        let _ = res;
        Ok(())
    }

    /// Return the most recent scrollback for a pane. Live panes read
    /// from the in-memory ring; closed panes read from `pane_lines`.
    /// `since_seq` (exclusive) lets the UI hydrate incrementally.
    pub async fn pane_lines(
        &self,
        pane_id: &str,
        since_seq: Option<i64>,
        pool: &DbPool,
    ) -> Result<Vec<crate::models::PaneLine>, AppError> {
        let live = {
            let panes = self.inner.panes.lock().await;
            panes.get(pane_id).cloned()
        };

        if let Some(state) = live {
            // Snapshot the ring under the inner mutex.
            let snapshot = tokio::task::spawn_blocking(move || -> Vec<crate::models::PaneLine> {
                let guard = match state.lock() {
                    Ok(g) => g,
                    Err(_) => return Vec::new(),
                };
                guard
                    .ring
                    .iter()
                    .map(|l| crate::models::PaneLine {
                        seq: l.seq,
                        k: l.kind.to_string(),
                        text: l.text.clone(),
                    })
                    .collect()
            })
            .await
            .map_err(|e| AppError::Internal(format!("spawn_blocking joined err: {e}")))?;

            // Tail-filter — preserves order; the ring is already in
            // ascending `seq` order.
            let cutoff = since_seq.unwrap_or(0);
            return Ok(snapshot.into_iter().filter(|l| l.seq > cutoff).collect());
        }

        // Closed pane → fall back to `pane_lines` table. Confirm
        // existence so the frontend can render a "no such pane" empty
        // state instead of an empty list ambiguously.
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM panes WHERE id = ?")
            .bind(pane_id)
            .fetch_optional(pool)
            .await?;
        if exists.is_none() {
            return Err(AppError::NotFound(format!("Pane {pane_id} not found")));
        }

        let cutoff = since_seq.unwrap_or(0);
        let rows = sqlx::query_as::<_, crate::models::PaneLine>(
            "SELECT seq, k, text FROM pane_lines \
             WHERE pane_id = ? AND seq > ? \
             ORDER BY seq ASC",
        )
        .bind(pane_id)
        .bind(cutoff)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }

    /// Kill every alive pane on app exit AND persist its scrollback
    /// to `pane_lines` synchronously before returning. Called from the
    /// `RunEvent::ExitRequested` hook in `lib.rs`.
    ///
    /// Per pane we:
    /// 1. Kill the child (via the cloned `ChildKiller`).
    /// 2. Drop master + writer so any blocking reader can unblock
    ///    (mandatory for Windows ConPTY).
    /// 3. Snapshot the in-memory ring buffer.
    /// 4. `INSERT` each ring line into `pane_lines` inside one tx.
    /// 5. `UPDATE panes SET status='closed', closed_at=…`.
    ///
    /// Steps 4–5 used to be deferred to the per-pane `run_waiter` task
    /// (via `tauri::async_runtime::spawn`). On app exit those tasks
    /// rarely won the race against runtime tear-down, so scrollback
    /// was lost and panes stayed `running` in the DB. See report.md §K1.
    pub async fn shutdown_all(&self, pool: &DbPool) {
        let panes: Vec<(String, Arc<Mutex<PaneState>>)> = {
            let mut panes = self.inner.panes.lock().await;
            panes.drain().collect()
        };

        if panes.is_empty() {
            return;
        }

        for (pane_id, state) in panes {
            // Sync section: under the per-pane lock, kill the child,
            // drop pipes, and snapshot the ring. Done on a blocking
            // thread because `ChildKiller::kill()` and pipe drops can
            // touch Win32 handles.
            let snapshot: Vec<RingLine> = match tokio::task::spawn_blocking({
                let state = Arc::clone(&state);
                move || -> Vec<RingLine> {
                    match state.lock() {
                        Ok(mut guard) => {
                            let _ = guard.killer.kill();
                            guard.master.take();
                            guard.writer.take();
                            guard.ring.iter().cloned().collect()
                        }
                        Err(_) => Vec::new(),
                    }
                }
            })
            .await
            {
                Ok(v) => v,
                Err(_) => Vec::new(),
            };

            // Async section: write directly to SQLite from this task
            // — no detached spawn that the runtime might not run.
            if let Err(e) = flush_ring_to_db(pool, &pane_id, &snapshot).await {
                tracing::error!(
                    pane_id = %pane_id,
                    error = %e,
                    "shutdown_all ring flush failed"
                );
            }
            if let Err(e) = sqlx::query(
                "UPDATE panes SET status = 'closed', closed_at = strftime('%s','now') \
                 WHERE id = ? AND closed_at IS NULL",
            )
            .bind(&pane_id)
            .execute(pool)
            .await
            {
                tracing::error!(
                    pane_id = %pane_id,
                    error = %e,
                    "shutdown_all finalise failed"
                );
            }
        }
    }
}

impl Default for TerminalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

