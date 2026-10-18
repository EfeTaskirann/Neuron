//! These tests run against ephemeral on-disk SQLite files (sqlx
//! does not load `:memory:` migrations in shared mode reliably on
//! Windows, so a tempdir is the simplest correct path).
use super::seed::{seed_demo_canvas, seed_demo_workflow, seed_mcp_servers};
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
        "orchestrator_messages",
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
    assert_eq!(names.len(), 16);
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
    assert_eq!(count, 1, "one migration recorded (0001_baseline)");
}

/// The sqlx ledger table, as sqlx creates it.
const LEDGER_DDL: &str = "CREATE TABLE _sqlx_migrations ( \
    version BIGINT PRIMARY KEY, description TEXT NOT NULL, \
    installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
    success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)";

#[test]
fn db_files_remove_the_sidecars_before_the_main_file() {
    let files = db_files(std::path::Path::new("dir").join("neuron.db").as_path());
    let names: Vec<String> = files
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["neuron.db-wal", "neuron.db-shm", "neuron.db"]);
}

/// A database created before the baseline (its ledger starts with the
/// old `0001_init`) is copied aside — with its data — and removed, and
/// a fresh database then takes the baseline.
#[tokio::test]
async fn a_pre_baseline_database_is_copied_aside_and_replaced() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().join("neuron.db");
    let pool = open_pool_at(&path).await.expect("open old db");
    sqlx::query(LEDGER_DDL).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (1, 'init', 1, x'00', 0), (13, 'servers drop mock stats', 1, x'00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO settings VALUES ('user.name', 'kept in the copy')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let copy = retire_pre_baseline_db(&path)
        .await
        .expect("retire")
        .expect("an old-chain database is moved aside");
    for f in db_files(&path) {
        assert!(!f.exists(), "{} still there", f.display());
    }
    let name = copy.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("neuron.pre-baseline-") && name.ends_with(".db"), "{name}");

    // The copy is a self-contained database holding the old data.
    let old = open_pool_at(&copy).await.expect("open copy");
    let kept: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'user.name'")
        .fetch_one(&old)
        .await
        .unwrap();
    assert_eq!(kept, "kept in the copy");
    old.close().await;

    // A fresh database at the old path takes the baseline cleanly.
    let fresh = open_pool_at(&path).await.expect("open fresh");
    MIGRATOR.run(&fresh).await.expect("baseline applies");
    let ledger: Vec<String> = sqlx::query_scalar("SELECT description FROM _sqlx_migrations")
        .fetch_all(&fresh)
        .await
        .unwrap();
    assert_eq!(ledger, vec!["baseline".to_string()]);
}

/// A baseline-lineage database, one with no ledger, and a missing file
/// are all left exactly as they are.
#[tokio::test]
async fn other_databases_are_left_alone() {
    let dir = tempfile::tempdir().expect("create tempdir");

    let missing = dir.path().join("missing.db");
    assert!(retire_pre_baseline_db(&missing).await.unwrap().is_none());
    assert!(!missing.exists());

    let current = dir.path().join("current.db");
    let pool = open_pool_at(&current).await.unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    sqlx::query("UPDATE settings SET value = 'mine' WHERE key = 'user.name'")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(retire_pre_baseline_db(&current).await.unwrap().is_none());
    let pool = open_pool_at(&current).await.unwrap();
    let name: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'user.name'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "mine");
    pool.close().await;

    let no_ledger = dir.path().join("plain.db");
    let pool = open_pool_at(&no_ledger).await.unwrap();
    sqlx::query("CREATE TABLE t (x INTEGER)").execute(&pool).await.unwrap();
    pool.close().await;
    assert!(retire_pre_baseline_db(&no_ledger).await.unwrap().is_none());
    assert!(no_ledger.exists());
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
async fn seed_mcp_servers_mirrors_the_bundled_catalog() {
    let (pool, _dir) = fresh_pool().await;
    seed_mcp_servers(&pool).await.expect("seed first time");
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM servers ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids, vec!["filesystem".to_string()], "only the bundled catalog");

    // The state an older build leaves behind: the user installed
    // Filesystem (tools recorded), its description has since been
    // corrected in the manifest, and a catalog-only stub is still there.
    sqlx::query(
        "UPDATE servers SET installed = 1, description = 'stale text' WHERE id = 'filesystem'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO servers (id, name, \"by\", description, installed) \
         VALUES ('github', 'GitHub', 'GitHub', 'stub', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO server_tools (server_id, name, description) VALUES \
         ('filesystem', 'read_file', 'x'), ('github', 'list_issues', 'y')",
    )
    .execute(&pool)
    .await
    .unwrap();

    seed_mcp_servers(&pool).await.expect("seed second time");
    let rows: Vec<(String, String, bool)> =
        sqlx::query_as("SELECT id, description, installed FROM servers ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    let manifest = crate::mcp::manifests::get("filesystem").unwrap().unwrap();
    assert_eq!(
        rows,
        vec![("filesystem".to_string(), manifest.description, true)],
        "stub deleted, description refreshed, install state kept"
    );
    let tools: Vec<String> = sqlx::query_scalar("SELECT server_id FROM server_tools")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(tools, vec!["filesystem".to_string()], "the stub's tools go with it");
}

/// The servers table no longer carries the design mock's invented
/// install counts, ratings and "featured" flag.
#[tokio::test]
async fn servers_table_has_no_mock_stats() {
    let (pool, _dir) = fresh_pool().await;
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('servers')")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(columns, ["id", "name", "by", "description", "installed"]);
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
