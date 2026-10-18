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
//! - Move aside a database created before the migrations were squashed
//!   into one baseline (see [`retire_pre_baseline_db`]).
//!
//! Per Charter §"Tech stack" SQLite is the single source of truth and
//! the ORM lives in Rust — the frontend never opens this database.

use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{ConnectOptions, Connection, Executor, SqlitePool};
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

/// `_sqlx_migrations.description` of version 1 in a database created
/// before the migrations were squashed into `0001_baseline.sql`
/// (2026-10-18): the old chain began with `0001_init.sql`.
const PRE_BASELINE_FIRST_MIGRATION: &str = "init";

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

    #[error("could not remove {path:?} after copying the pre-baseline database aside: {source}")]
    Retire {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
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
    if let Some(copy) = retire_pre_baseline_db(&path).await? {
        tracing::warn!(
            copy = %copy.display(),
            "database predates the migration baseline: kept a copy and started a fresh one"
        );
    }
    let pool = open_pool_at(&path).await?;
    MIGRATOR.run(&pool).await?;
    seed_demo_workflow(&pool).await?;
    seed_demo_canvas(&pool).await?;
    seed_mcp_servers(&pool).await?;
    Ok(pool)
}

/// The migrations were squashed into one baseline (`0001_baseline.sql`,
/// 2026-10-18). A database created before that recorded the old chain
/// (`0001_init` …); the baseline can neither upgrade it nor run against
/// it (sqlx refuses a changed version 1). Such a database is copied to
/// `neuron.pre-baseline-<ulid>.db` beside it — `VACUUM INTO` writes one
/// self-contained file, WAL contents included — and then removed, so
/// `init` starts a fresh one. Returns the copy's path when it did so;
/// any other database (none yet, no ledger, the baseline lineage) is
/// left untouched.
async fn retire_pre_baseline_db(path: &Path) -> Result<Option<PathBuf>, DbError> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut conn = SqliteConnectOptions::new()
        .filename(path)
        .disable_statement_logging()
        .connect()
        .await?;
    let has_ledger: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master \
         WHERE type = 'table' AND name = '_sqlx_migrations')",
    )
    .fetch_one(&mut conn)
    .await?;
    let old_chain = has_ledger
        && sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM _sqlx_migrations \
             WHERE version = 1 AND description = ?)",
        )
        .bind(PRE_BASELINE_FIRST_MIGRATION)
        .fetch_one(&mut conn)
        .await?;
    if !old_chain {
        conn.close().await?;
        return Ok(None);
    }
    let copy = path.with_file_name(format!("neuron.pre-baseline-{}.db", ulid::Ulid::new()));
    sqlx::query("VACUUM INTO ?")
        .bind(copy.to_string_lossy().into_owned())
        .execute(&mut conn)
        .await?;
    conn.close().await?;
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        let file = PathBuf::from(file);
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(DbError::Retire { path: file, source }),
        }
    }
    Ok(Some(copy))
}

/// Build a pool against a concrete on-disk path. Split out so tests
/// can call it with a `tempdir()` location without going through
/// `AppHandle`.
async fn open_pool_at(path: &Path) -> Result<DbPool, DbError> {
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
