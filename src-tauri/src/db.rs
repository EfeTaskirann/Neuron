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

    /// Acceptance: all expected schema tables exist after migration.
    /// The list grows as migrations land — keep `expected` sorted
    /// alphabetically and update both arms (the array + length
    /// assertion) when adding a new file under `migrations/`.
    #[tokio::test]
    async fn migration_creates_all_expected_tables() {
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
            "settings",
            "swarm_jobs",
            "swarm_stages",
            "swarm_workspace_locks",
            "workflows",
        ];
        assert_eq!(
            names,
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "schema tables drift — re-run `cargo sqlx prepare` after \
             updating migrations and update this test"
        );
        assert_eq!(names.len(), 15);
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
        // Migration count grows as the schema evolves. Update this
        // when adding a new file under `migrations/`.
        assert_eq!(
            count, 8,
            "eight migrations recorded (0001 + 0002 + 0003 + 0004 + 0005 + 0006 + 0007 + 0008)"
        );
    }

    /// Acceptance: WP-W2-05 — seed_mcp_servers writes every bundled
    /// manifest on first call and is a no-op on subsequent calls.
    /// `seed_demo_canvas` lays down 6 nodes + 6 edges for the
    /// `daily-summary` workflow exactly once, even when run again
    /// after a relaunch. The status column on a node may have been
    /// updated by the runtime in between (e.g. `running`→`success`);
    /// `INSERT OR IGNORE` must preserve those user-/runtime-driven
    /// updates rather than reverting to the seed value.
    #[tokio::test]
    async fn seed_demo_canvas_is_idempotent() {
        let (pool, _dir) = fresh_pool().await;
        seed_demo_workflow(&pool).await.expect("workflow seed");
        seed_demo_canvas(&pool).await.expect("canvas seed first time");

        let (n, e): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM nodes), (SELECT COUNT(*) FROM edges)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 6);
        assert_eq!(e, 6);

        // Mutate a node's status in between — re-seed must NOT revert.
        sqlx::query("UPDATE nodes SET status = 'idle' WHERE id = 'n4'")
            .execute(&pool)
            .await
            .unwrap();
        seed_demo_canvas(&pool).await.expect("canvas seed second time");
        let (n2, e2): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM nodes), (SELECT COUNT(*) FROM edges)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n2, 6, "no duplicate nodes after re-seed");
        assert_eq!(e2, 6, "no duplicate edges after re-seed");
        let n4_status: String =
            sqlx::query_scalar("SELECT status FROM nodes WHERE id='n4'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(n4_status, "idle", "runtime status must survive re-seed");
    }

    #[tokio::test]
    async fn seed_mcp_servers_is_idempotent() {
        let (pool, _dir) = fresh_pool().await;
        seed_mcp_servers(&pool).await.expect("seed first time");
        let after_first: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM servers")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(after_first, 12, "all twelve manifests seeded");

        // Re-running must not duplicate or overwrite — flip one row's
        // `installed` to 1 first and confirm it survives the re-seed.
        sqlx::query("UPDATE servers SET installed=1 WHERE id='filesystem'")
            .execute(&pool)
            .await
            .unwrap();
        seed_mcp_servers(&pool).await.expect("seed second time");
        let after_second: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM servers")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(after_second, 12, "no duplicates after re-seed");
        let installed: bool =
            sqlx::query_scalar("SELECT installed FROM servers WHERE id='filesystem'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            installed,
            "user-toggled `installed` flag must survive re-seed"
        );

        // Acceptance: ids match the canonical twelve (data.js#servers
        // parity per Charter Constraint #1).
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM servers ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        let mut expected = vec![
            "browser",
            "figma",
            "filesystem",
            "github",
            "linear",
            "memory",
            "notion",
            "postgres",
            "sentry",
            "slack",
            "stripe",
            "vector-db",
        ];
        expected.sort();
        assert_eq!(
            ids,
            expected.into_iter().map(String::from).collect::<Vec<_>>(),
            "twelve canonical MCP servers"
        );
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
