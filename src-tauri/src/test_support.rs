//! WP-W2-03 test fixtures.
//!
//! Hoisted from `db::tests::fresh_pool` so every command module's
//! `#[cfg(test)]` block can build a freshly-migrated SQLite pool
//! without copy-paste. Cargo allows `#[cfg(test)] pub mod ...` from
//! `lib.rs`, and the resulting `crate::test_support::*` paths stay
//! invisible in release builds.
//!
//! Each fixture returns the pool **and** the owning `TempDir` —
//! callers must keep the temp dir alive for the lifetime of the
//! test, otherwise the SQLite file is unlinked early on Windows and
//! later queries fail with "database is locked / unable to open
//! database file".

#![cfg(test)]
#![allow(dead_code)]

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{ConnectOptions, Executor};

use crate::db::{DbPool, MIGRATOR};

/// Spin up a fresh pool against a unique temp path with all schema
/// migrations applied. Mirrors `db::tests::fresh_pool` byte-for-byte
/// so the migration semantics tested in WP-W2-02 still cover this
/// path.
pub async fn fresh_pool() -> (DbPool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().join("neuron-test.db");
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .disable_statement_logging();
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                conn.execute("PRAGMA foreign_keys = ON;").await?;
                Ok(())
            })
        })
        .connect_with(opts)
        .await
        .expect("open pool");
    MIGRATOR.run(&pool).await.expect("run migrations");
    (pool, dir)
}

/// Insert one workflow row plus the agent rows referenced by the
/// majority of command tests. Most happy-path tests need at least
/// "an agent exists" or "a workflow exists" to thread foreign keys
/// without re-implementing seeding inline.
pub async fn seed_minimal(pool: &DbPool) {
    sqlx::query(
        "INSERT INTO agents (id, name, model, temp, role) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind("a1")
    .bind("Planner")
    .bind("gpt-4o")
    .bind(0.4_f64)
    .bind("Breaks the goal into ordered subtasks.")
    .execute(pool)
    .await
    .expect("seed agent");

    sqlx::query("INSERT INTO workflows (id, name) VALUES (?, ?)")
        .bind("w1")
        .bind("Daily summary")
        .execute(pool)
        .await
        .expect("seed workflow");
}

/// Insert one MCP server with `installed=0` so install/uninstall
/// tests have a known starting state.
pub async fn seed_server_uninstalled(pool: &DbPool) {
    sqlx::query(
        "INSERT INTO servers (id, name, by, description, installs, rating, featured, installed) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
    )
    .bind("s3")
    .bind("PostgreSQL")
    .bind("Anthropic")
    .bind("Query relational databases.")
    .bind(8100_i64)
    .bind(4.8_f64)
    .bind(0_i64)
    .execute(pool)
    .await
    .expect("seed server");
}

/// Insert one pane row with `status='idle'` for terminal:list / kill
/// tests.
pub async fn seed_pane(pool: &DbPool, id: &str) {
    sqlx::query(
        "INSERT INTO panes (id, workspace, agent_kind, role, cwd, status, pid) \
         VALUES (?, 'personal', 'shell', NULL, '/tmp', 'idle', NULL)",
    )
    .bind(id)
    .execute(pool)
    .await
    .expect("seed pane");
}

/// Stand up a `tauri::test::MockRuntime` app with a freshly-migrated
/// pool already in `app.state::<DbPool>()`. Hoisted from the six
/// per-module copies that previously diverged in subtle ways
/// (`max_connections`, post-build `manage` order). Single source of
/// truth — every command-test that does not need extra app-state can
/// call this directly.
///
/// We intentionally use `mock_context(noop_assets())` instead of
/// `tauri::generate_context!()` so the test exe doesn't bundle the
/// production frontend dist, icons, and permissions. The generated
/// context drags in tens of MB of asset bytes that trip Windows
/// `STATUS_ENTRYPOINT_NOT_FOUND` when the test binary tries to load.
pub async fn mock_app_with_pool() -> (
    tauri::App<tauri::test::MockRuntime>,
    DbPool,
    tempfile::TempDir,
) {
    let (pool, dir) = fresh_pool().await;
    let app = tauri::test::mock_builder()
        .manage(pool.clone())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app");
    (app, pool, dir)
}

/// Same as [`mock_app_with_pool`] but also installs an empty
/// `TerminalRegistry` in app state — required by the `terminal::*`
/// command module's tests. Kept as a separate helper rather than
/// folded into the default so the other five command modules' test
/// binaries don't pull in the PTY supervisor's dependency tree.
pub async fn mock_app_with_pool_and_terminal_registry() -> (
    tauri::App<tauri::test::MockRuntime>,
    DbPool,
    tempfile::TempDir,
) {
    let (pool, dir) = fresh_pool().await;
    let app = tauri::test::mock_builder()
        .manage(pool.clone())
        .manage(crate::sidecar::terminal::TerminalRegistry::new())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app");
    (app, pool, dir)
}
