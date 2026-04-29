//! WP-W2-02 — SQLite + sqlx wiring.
//!
//! Responsibilities
//! ----------------
//! - Resolve the on-disk database path under Tauri's per-app data dir.
//! - Open a `SqlitePool` with WAL journaling and `foreign_keys = ON`
//!   enforced on every new connection.
//! - Run all bundled migrations from `migrations/`. The migrator records
//!   applied versions in `_sqlx_migrations`, so calling `init` on every
//!   app launch is a no-op after the first run (the "idempotent" gate).
//!
//! Per Charter §"Tech stack" SQLite is the single source of truth and
//! the ORM lives in Rust — the frontend never opens this database.

use std::path::PathBuf;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{ConnectOptions, Executor, SqlitePool};
use tauri::{AppHandle, Manager};
use thiserror::Error;

/// Embedded migrator. `sqlx::migrate!` reads `migrations/` at compile
/// time so the binary is self-contained — no migration files need to
/// ship with the installer.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Type alias used by `tauri::State<DbPool>` consumers throughout the
/// `commands::` modules. Exporting it here lets command files depend
/// on a stable name even if we ever swap the underlying pool.
pub type DbPool = SqlitePool;

/// Database file name placed under the app data dir.
const DB_FILENAME: &str = "neuron.db";

/// Errors surfaced by `db::init` and by code that touches the pool
/// during startup. Tauri commands generally surface their own
/// per-command error types; this one is for the bootstrap path.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("could not resolve Tauri app data dir: {0}")]
    AppDataDir(#[from] tauri::Error),

    #[error("could not create app data dir at {path:?}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Open (or create) the on-disk Neuron database, run migrations, and
/// return a pool that callers can `app.manage(...)` so Tauri commands
/// can inject it via `State<DbPool>`.
///
/// `create_if_missing` makes the very first launch idempotent with
/// later launches; the migrator handles "already-applied" tracking.
pub async fn init(app: &AppHandle) -> Result<DbPool, DbError> {
    let dir = app.path().app_data_dir()?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|source| DbError::CreateDir {
            path: dir.clone(),
            source,
        })?;
    }
    let path = dir.join(DB_FILENAME);
    let pool = open_pool_at(&path).await?;
    MIGRATOR.run(&pool).await?;
    seed_demo_workflow(&pool).await?;
    Ok(pool)
}

/// Idempotently insert the `daily-summary` workflow row that the
/// LangGraph Python sidecar (WP-W2-04) hardcodes. Without this row
/// `runs:create('daily-summary')` fails the workflow-existence check
/// before it ever reaches the sidecar — which makes the WP-04 manual
/// smoke test unreachable on a fresh DB. WP-W2-02 deliberately scoped
/// "seed data" out (deferring to WP-W2-08 fixtures); this is the
/// minimum viable seed needed to make WP-W2-04's acceptance gate
/// reachable from the running app, not a general fixture seed.
///
/// `INSERT OR IGNORE` keeps it safe across relaunches and across
/// future WP-W2-08 fixture work that may insert the same row.
async fn seed_demo_workflow(pool: &DbPool) -> Result<(), DbError> {
    sqlx::query(
        "INSERT OR IGNORE INTO workflows (id, name) VALUES ('daily-summary', 'Daily summary')",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Build a pool against a concrete on-disk path. Split out so tests
/// can call it with a `tempdir()` location without going through
/// `AppHandle`.
async fn open_pool_at(path: &std::path::Path) -> Result<DbPool, DbError> {
    // WAL gives us concurrent readers + a single writer, which is the
    // shape every Neuron command surface needs (UI reads while the
    // runtime appends spans).
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .disable_statement_logging();

    SqlitePoolOptions::new()
        .max_connections(8)
        // Defensive belt-and-suspenders: `foreign_keys` on the connect
        // options *should* be enough, but SQLite is famously laissez-
        // faire about pragmas. Re-asserting at handout time guarantees
        // every connection the pool hands out has FKs enforced.
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                conn.execute("PRAGMA foreign_keys = ON;").await?;
                Ok(())
            })
        })
        .connect_with(opts)
        .await
        .map_err(DbError::from)
}

#[cfg(test)]
mod tests {
    //! These tests run against ephemeral on-disk SQLite files (sqlx
    //! does not load `:memory:` migrations in shared mode reliably on
    //! Windows, so a tempdir is the simplest correct path).
    use super::*;

    /// Spin up a fresh pool against a unique temp path. Callers should
    /// keep the returned `tempfile::TempDir` alive for the duration of
    /// the test so the file isn't unlinked early.
    async fn fresh_pool() -> (DbPool, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("neuron-test.db");
        let pool = open_pool_at(&path).await.expect("open pool");
        MIGRATOR.run(&pool).await.expect("run migrations");
        (pool, dir)
    }

    /// Acceptance: all 11 schema tables exist after migration.
    #[tokio::test]
    async fn migration_creates_all_eleven_tables() {
        let (pool, _dir) = fresh_pool().await;
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND name NOT LIKE 'sqlite_%' \
               AND name NOT LIKE '_sqlx_%' \
             ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .expect("list tables");
        let expected = [
            "agents",
            "edges",
            "mailbox",
            "nodes",
            "pane_lines",
            "panes",
            "runs",
            "runs_spans",
            "server_tools",
            "servers",
            "workflows",
        ];
        assert_eq!(
            names,
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "schema tables drift — re-run `cargo sqlx prepare` after \
             updating migrations and update this test"
        );
        assert_eq!(names.len(), 11);
    }

    /// Acceptance: every connection the pool hands out enforces FKs.
    #[tokio::test]
    async fn pragma_foreign_keys_is_on_per_connection() {
        let (pool, _dir) = fresh_pool().await;
        for _ in 0..3 {
            let on: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(&pool)
                .await
                .expect("read pragma");
            assert_eq!(on, 1, "PRAGMA foreign_keys must be 1");
        }
    }

    /// Smoke: the compile-time-checked macro variant works against the
    /// offline cache committed at `src-tauri/.sqlx/`. This guarantees
    /// the offline pipeline is wired correctly for WP-W2-03; if a
    /// future schema change invalidates the cache, this test will
    /// fail at compile time, not at runtime.
    #[tokio::test]
    async fn macro_query_uses_offline_cache() {
        let (pool, _dir) = fresh_pool().await;
        let count: i64 = sqlx::query_scalar!("SELECT COUNT(*) FROM agents")
            .fetch_one(&pool)
            .await
            .expect("count via macro");
        assert_eq!(count, 0);
    }

    /// Acceptance: re-running `init` on the same DB is a no-op (the
    /// migrator's bookkeeping table makes this safe).
    #[tokio::test]
    async fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("idem.db");

        let pool1 = open_pool_at(&path).await.expect("open pool 1");
        MIGRATOR.run(&pool1).await.expect("first migrate");
        // Simulate a second app launch.
        MIGRATOR.run(&pool1).await.expect("re-run is a no-op");
        pool1.close().await;

        // And a fresh pool against the same file should also see no
        // pending migrations.
        let pool2 = open_pool_at(&path).await.expect("open pool 2");
        MIGRATOR
            .run(&pool2)
            .await
            .expect("second-launch migrate must not error");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&pool2)
            .await
            .expect("count applied migrations");
        assert_eq!(count, 1, "exactly one migration recorded");
    }

    /// Acceptance: the pool actually works end-to-end — insert and
    /// read back a row from one of the migrated tables.
    #[tokio::test]
    async fn pool_can_insert_and_select() {
        let (pool, _dir) = fresh_pool().await;
        sqlx::query(
            "INSERT INTO agents (id, name, model, temp, role) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind("a1")
        .bind("Planner")
        .bind("gpt-4o")
        .bind(0.4_f64)
        .bind("Breaks the goal into ordered subtasks.")
        .execute(&pool)
        .await
        .expect("insert agent");

        let (id, name, model): (String, String, String) =
            sqlx::query_as("SELECT id, name, model FROM agents WHERE id = ?")
                .bind("a1")
                .fetch_one(&pool)
                .await
                .expect("select agent");
        assert_eq!(id, "a1");
        assert_eq!(name, "Planner");
        assert_eq!(model, "gpt-4o");
    }
}
