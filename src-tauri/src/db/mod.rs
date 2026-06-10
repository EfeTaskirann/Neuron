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

mod seed;

use seed::{seed_demo_canvas, seed_demo_workflow, seed_mcp_servers};

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
    seed_demo_canvas(&pool).await?;
    seed_mcp_servers(&pool).await?;
    Ok(pool)
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
mod tests;
