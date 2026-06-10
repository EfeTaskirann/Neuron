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
    seed_demo_canvas(&pool).await?;
    seed_mcp_servers(&pool).await?;
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

/// Idempotently seed the 6 nodes + 6 edges that constitute the
/// `daily-summary` canvas. WP-W2-08 §"Risks" called for this fixture
/// — without it `workflows:get('daily-summary')` returns
/// `nodes: []`, `edges: []` and the Canvas route renders empty.
///
/// Coordinates / labels / statuses copy `Neuron Design/app/canvas.jsx`
/// verbatim so the rendered canvas matches the design-mock vibe
/// post-migration. `INSERT OR IGNORE` keeps it safe across relaunches.
async fn seed_demo_canvas(pool: &DbPool) -> Result<(), DbError> {
    let nodes: &[(&str, &str, i64, i64, &str, &str, &str)] = &[
        ("n1", "llm",   60,  80,  "Planner",    "gpt-4o · 1.2k tok",   "success"),
        ("n2", "tool",  360, 40,  "fetch_docs", "tool · 0.34s",        "success"),
        ("n3", "tool",  360, 200, "search_web", "tool · 0.52s",        "success"),
        ("n4", "llm",   660, 110, "Reasoner",   "gpt-4o · 2.4k tok",   "running"),
        ("n5", "human", 960, 70,  "Approve",    "human · waiting",     "waiting"),
        ("n6", "logic", 960, 220, "Route",      "logic · idle",        "idle"),
    ];
    for (id, kind, x, y, title, meta, status) in nodes {
        sqlx::query(
            "INSERT OR IGNORE INTO nodes \
             (id, workflow_id, kind, x, y, title, meta, status) \
             VALUES (?, 'daily-summary', ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(kind)
        .bind(x)
        .bind(y)
        .bind(title)
        .bind(meta)
        .bind(status)
        .execute(pool)
        .await?;
    }

    // (id, from, to, active)
    let edges: &[(&str, &str, &str, i64)] = &[
        ("e1", "n1", "n2", 0),
        ("e2", "n1", "n3", 0),
        ("e3", "n2", "n4", 1),
        ("e4", "n3", "n4", 1),
        ("e5", "n4", "n5", 0),
        ("e6", "n4", "n6", 0),
    ];
    for (id, from_node, to_node, active) in edges {
        sqlx::query(
            "INSERT OR IGNORE INTO edges \
             (id, workflow_id, from_node, to_node, active) \
             VALUES (?, 'daily-summary', ?, ?, ?)",
        )
        .bind(id)
        .bind(from_node)
        .bind(to_node)
        .bind(active)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Idempotently seed the six MCP servers bundled in
/// `src/mcp/manifests/`. WP-W2-05 §"Notes / risks":
///
/// > Seeded server manifests live in `src-tauri/src/mcp/manifests/` as
/// > JSON. Loaded by migration `0002_seed_mcp.sql` via Rust seed
/// > function (not raw SQL — too brittle).
///
/// We elide the migration file entirely (the seed is data-dependent
/// on the JSON manifests) and run a Rust seed function from `init`.
/// `INSERT OR IGNORE` keeps it safe across relaunches; the user-
/// facing `installs`/`rating`/`featured`/`installed` values do not
/// drift on re-seed (we never overwrite them — the user may have
/// already toggled `installed=1`).
///
/// **Failure handling.** A single corrupted manifest JSON used to
/// abort the whole startup path (the panic landed in
/// `tauri::Builder::build().expect(...)`, so the app refused to
/// launch on every relaunch — see report.md §K6). We now soft-load
/// the catalog: per-file parse failures are logged via `tracing::warn!`
/// but the surviving manifests are seeded. Each `mcp:install`
/// against a failed id will still surface a `ManifestError` to the
/// caller; the rest of the app stays usable.
async fn seed_mcp_servers(pool: &DbPool) -> Result<(), DbError> {
    let report = crate::mcp::manifests::parse_report();
    for failure in &report.failures {
        tracing::warn!(
            file_key = %failure.file_key,
            error = %failure.error,
            "skipping bundled MCP manifest"
        );
    }
    for m in &report.manifests {
        sqlx::query(
            "INSERT OR IGNORE INTO servers \
             (id, name, by, description, installs, rating, featured, installed) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
        )
        .bind(&m.id)
        .bind(&m.name)
        .bind(&m.by)
        .bind(&m.description)
        .bind(m.installs)
        .bind(m.rating)
        .bind(m.featured as i64)
        .execute(pool)
        .await?;
    }
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
mod tests;
