---
id: WP-W3-12k2
title: Orchestrator persistent chat history (SQLite-backed conversation context)
owner: TBD
status: not-started
depends-on: [WP-W3-12k1, WP-W3-12k3]
acceptance-gate: "Per-workspace chat history persists across app restarts. `swarm:orchestrator_decide` injects the most-recent N messages into the Orchestrator's prompt so it can give context-aware decisions. New `swarm:orchestrator_history(workspace_id, limit?)` IPC seeds the chat panel on mount. New migration `0009_orchestrator_messages.sql`."
---

## Goal

Close the 9-agent vision's last polish gap. Today (post-W3-12k-3)
the Orchestrator chat panel is functional but **stateless** —
each user message is independent. Reload = empty chat. Multi-
message context ("I want to refactor auth" → "/me endpoint")
is lost.

This WP adds:
1. SQLite persistence for chat messages per workspace.
2. Context-aware Orchestrator decisions (recent N messages
   piped into the prompt).
3. UI seed-from-DB on mount so reload preserves history.

## Why now

W3-12k-1 + W3-12k-3 made the Orchestrator usable; this WP makes
it useful for real conversations. A user discussing a refactor
across 3-5 messages benefits enormously from context.

## Charter alignment

No tech-stack change. New SQLite table + sqlx queries; existing
W3-12b persistence patterns extended.

## Scope

### 1. Migration `0009_orchestrator_messages.sql`

```sql
CREATE TABLE orchestrator_messages (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  workspace_id    TEXT    NOT NULL,
  role            TEXT    NOT NULL,    -- "user" | "orchestrator" | "job"
  -- For role=user: content = raw user text.
  -- For role=orchestrator: content = JSON-serialized OrchestratorOutcome
  --   (action + text + reasoning packed for round-trip).
  -- For role=job: content = job_id; goal carried in goal column.
  content         TEXT    NOT NULL,
  goal            TEXT,                -- nullable; populated for role=job
  created_at_ms   INTEGER NOT NULL
);

CREATE INDEX idx_orchestrator_messages_workspace
  ON orchestrator_messages (workspace_id, created_at_ms);
```

Migration count goes 8 → 9. Update `db.rs` count tests.

The single TEXT `content` column with role-based interpretation
keeps the schema simple at the cost of role-aware parsing. An
alternative (dedicated columns per shape) would balloon the
schema for marginal gain.

### 2. `swarm/coordinator/orchestrator_session.rs`

New module sibling to `orchestrator.rs`:

```rust
use sqlx::SqlitePool;
use crate::db::DbPool;
use crate::error::AppError;
use super::orchestrator::{OrchestratorAction, OrchestratorOutcome};

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorMessageRole {
    User,
    Orchestrator,
    Job,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct OrchestratorMessage {
    pub id: i64,
    pub workspace_id: String,
    pub role: OrchestratorMessageRole,
    /// Free-form by role:
    /// - User: raw user text
    /// - Orchestrator: JSON-encoded OrchestratorOutcome
    /// - Job: job_id (string)
    pub content: String,
    pub goal: Option<String>,         // populated only for role=Job
    pub created_at_ms: i64,
}

pub(super) async fn append_user_message(
    pool: &DbPool,
    workspace_id: &str,
    text: &str,
    now_ms: i64,
) -> Result<i64, AppError>;

pub(super) async fn append_orchestrator_message(
    pool: &DbPool,
    workspace_id: &str,
    outcome: &OrchestratorOutcome,
    now_ms: i64,
) -> Result<i64, AppError>;

pub(super) async fn append_job_message(
    pool: &DbPool,
    workspace_id: &str,
    job_id: &str,
    goal: &str,
    now_ms: i64,
) -> Result<i64, AppError>;

pub(super) async fn list_recent_messages(
    pool: &DbPool,
    workspace_id: &str,
    limit: u32,
) -> Result<Vec<OrchestratorMessage>, AppError>;
```

`limit` defaults to 50 in callers. Cap at 200 hard via the IPC
to prevent runaway queries. SELECT ordered by `created_at_ms
DESC LIMIT ?` then reverse in-memory for chronological display.

### 3. `swarm:orchestrator_decide` extended

The W3-12k-1 implementation calls `transport.invoke(profile,
user_message, ...)`. Update to:

1. Load recent N=10 messages via `list_recent_messages(pool,
   workspace_id, 10)`.
2. Build a context-aware prompt: prepend a brief history
   summary BEFORE the user_message.

```rust
const HISTORY_TEMPLATE: &str = "Önceki konuşma (eskiyi yeniyi):\n\n\
{history_lines}\n\n\
---\n\nKullanıcının yeni mesajı:\n\n{user_message}\n";

fn render_with_history(
    history: &[OrchestratorMessage],
    user_message: &str,
) -> String {
    if history.is_empty() {
        return user_message.to_string();
    }
    let history_lines: Vec<String> = history.iter().map(|m| {
        match m.role {
            OrchestratorMessageRole::User => format!("[user]: {}", m.content),
            OrchestratorMessageRole::Orchestrator => {
                // Decode the JSON-packed outcome for human-readable display.
                if let Ok(outcome) = serde_json::from_str::<OrchestratorOutcome>(&m.content) {
                    format!("[orchestrator/{}]: {}", action_label(outcome.action), outcome.text)
                } else {
                    format!("[orchestrator]: {}", m.content)
                }
            }
            OrchestratorMessageRole::Job => format!(
                "[swarm dispatched]: {} (goal: {})",
                m.content,
                m.goal.as_deref().unwrap_or("-")
            ),
        }
    }).collect();
    HISTORY_TEMPLATE
        .replace("{history_lines}", &history_lines.join("\n"))
        .replace("{user_message}", user_message)
}
```

3. Pass the rendered prompt to `transport.invoke`.
4. Parse the `OrchestratorOutcome`.
5. Persist the user message (BEFORE the invoke call, so even if
   invoke fails the user's input is preserved) and the
   orchestrator outcome (AFTER successful parse) via the new
   `append_*_message` helpers.
6. Return the outcome.

### 4. New `swarm:orchestrator_history` IPC

```rust
#[tauri::command]
#[specta::specta]
pub async fn swarm_orchestrator_history<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
    limit: Option<u32>,
) -> Result<Vec<OrchestratorMessage>, AppError>;
```

Returns recent messages (default 50, capped 200), oldest-first
ordering. UI seeds the chat panel on mount.

### 5. New `swarm:orchestrator_clear_history` IPC

```rust
#[tauri::command]
#[specta::specta]
pub async fn swarm_orchestrator_clear_history<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
) -> Result<(), AppError>;
```

Hard-delete all messages for a workspace. UI exposes a "Clear
chat" button.

### 6. Job-message persistence

When `swarm:run_job` succeeds and the chat panel appends a
`{role: 'job', jobId, goal}` message, the frontend SHOULD
also persist it via a new IPC OR the FSM SHOULD persist it
itself when dispatched-by-orchestrator.

Pick the simpler path: **frontend persists** via a new
`swarm:orchestrator_log_job(workspace_id, job_id, goal)` IPC
called immediately after `swarm:run_job` returns. Three IPCs
becomes one chat-orchestrator interaction surface.

Or alternative: have a single `swarm:orchestrator_send_message`
IPC that wraps decide + run_job + log internally. Cleaner UX
but more backend complexity.

**Pick the simple path: 3 IPCs (decide / history / log_job).
Frontend orchestrates them.**

### 7. Frontend updates

`app/src/hooks/useOrchestratorHistory.ts`:

```typescript
export function useOrchestratorHistory(workspaceId: string) {
  return useQuery<OrchestratorMessage[]>({
    queryKey: ['orchestrator-history', workspaceId],
    queryFn: () => unwrap(commands.swarmOrchestratorHistory(workspaceId, null)),
    staleTime: Infinity,  // history loaded once on mount; mutations invalidate
  });
}
```

`app/src/hooks/useClearOrchestratorHistory.ts`: mutation hook.

`app/src/components/OrchestratorChatPanel.tsx` updates:
- On mount: read history via useOrchestratorHistory, seed
  `messages` state from it.
- After successful `decide` mutation: invalidate
  `['orchestrator-history']` so the next mount sees the new
  messages.
- New "Clear chat" button at the top of the chat history area
  that calls `useClearOrchestratorHistory().mutate(workspaceId)`,
  then resets local `messages` to empty AND invalidates the
  history query.

The chat panel's local `messages` state STILL drives the live
display (TanStack history is for the SEED + cross-session
durability). On mount, the seed populates `messages`; new
messages append to local state AND fire-and-forget
log-to-DB via the IPC.

### 8. Tests (unit, mock-driven)

#### Store layer
- `migration_0009_creates_orchestrator_messages_table`
- `migration_0009_indexes_workspace_and_created_at`
- `append_user_message_round_trip`
- `append_orchestrator_message_serializes_outcome_as_json`
- `append_job_message_populates_goal_column`
- `list_recent_messages_returns_chronological_oldest_first`
- `list_recent_messages_respects_limit`
- `list_recent_messages_filters_by_workspace_id`
- `clear_history_deletes_all_workspace_messages`
- `clear_history_leaves_other_workspaces_intact`

#### Prompt rendering
- `render_with_history_empty_returns_user_message_verbatim`
- `render_with_history_includes_user_role_label`
- `render_with_history_includes_orchestrator_action_label`
- `render_with_history_includes_dispatched_job_id`
- `render_with_history_handles_unparseable_orchestrator_content`

#### IPC
- `swarm_orchestrator_decide_appends_both_messages_to_history`
- `swarm_orchestrator_decide_uses_history_in_prompt`
- `swarm_orchestrator_history_returns_oldest_first`
- `swarm_orchestrator_history_respects_limit_cap_200`
- `swarm_orchestrator_clear_history_empties_workspace`

Frontend tests (Vitest):
- `useOrchestratorHistory_calls_swarmOrchestratorHistory_with_workspaceId`
- `OrchestratorChatPanel_seeds_messages_from_history_on_mount`
- `OrchestratorChatPanel_clear_chat_button_clears_local_and_invalidates_query`

Target: Rust 364 → 380+ (+15-20), frontend 45 → 48+ (+3).

### 9. Bindings regen

`pnpm gen:bindings` adds:
- `OrchestratorMessage` struct
- `OrchestratorMessageRole` enum
- `commands.swarmOrchestratorHistory(workspaceId, limit?)`
- `commands.swarmOrchestratorClearHistory(workspaceId)`
- `commands.swarmOrchestratorLogJob(workspaceId, jobId, goal)`

`pnpm gen:bindings:check` exit 0 post-commit.

## Out of scope

- ❌ Multi-workspace chat switching UI (workspaceId stays
  `"default"` per W3-14 / 12k-3 pattern).
- ❌ Per-message editing or deletion.
- ❌ Search across chat history.
- ❌ Streaming Orchestrator replies.
- ❌ Markdown rendering in bubbles (plain text).
- ❌ Multi-session UI (one chat per workspace, single thread).
- ❌ Trim policy for ancient messages (no soft delete /
  archival; `clear_history` is the only purge).

## Acceptance criteria

- [ ] Migration `0009_orchestrator_messages.sql` exists.
- [ ] Migration count test bumps 8 → 9.
- [ ] `swarm:orchestrator_history`, `swarm:orchestrator_clear_history`,
      `swarm:orchestrator_log_job` IPC commands compile.
- [ ] `swarm:orchestrator_decide` injects last 10 messages
      into the prompt and persists user + outcome messages.
- [ ] `OrchestratorChatPanel` seeds from history on mount,
      invalidates history on new messages, exposes Clear button.
- [ ] All Week-2 + Week-3-prior tests pass; target Rust ≥380,
      frontend ≥48.
- [ ] No new dep, no `unsafe`, no `eprintln!`.
- [ ] `bindings.ts` regenerated; `gen:bindings:check` exit 0
      post-commit.

## Verification commands

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

pnpm gen:bindings
pnpm gen:bindings:check
pnpm typecheck
pnpm test --run
pnpm lint
```

## Notes / risks

- **Prompt size growth** with long histories. 10 messages × ~500 chars = 5KB context overhead. Acceptable for typical conversations; cost not a concern per owner directive.
- **Stale outcome content** if persona changes mid-session. The history shows old action labels even if the persona's heuristics shifted. Acceptable; chat is conversational, not formal.
- **No TTL on messages**. Long-running installs accumulate history. `clear_history` is manual; future polish could add age-based trim.
- **Race between decide and log-job**. The frontend awaits decide → run_job → log_job sequentially. If log_job fails (DB busy), the in-memory bubble shows but the next mount won't see it. Document; acceptable for now.

## Sub-agent reminders

- Read this WP in full.
- Read `src-tauri/migrations/0008_swarm_decision.sql` for migration style.
- Read `src-tauri/src/swarm/coordinator/store.rs` (W3-12b/d/f) for sqlx::query string-query pattern; mirror for orchestrator_messages.
- Read `app/src/hooks/useSwarmJobs.ts` for the read-query hook pattern; useOrchestratorHistory mirrors it.
- DO NOT add a new dep.
- DO NOT use sqlx::query! macro (offline cache regen would be required); stay on runtime-checked sqlx::query.
- DO NOT cap limit at less than 200 (let users pull a lot if they want).
- DO NOT auto-trim old messages. clear_history is the only purge.
- Per AGENTS.md: one WP = one commit.
