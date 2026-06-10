//! First-launch seed data (T3-04/D2 consolidation) — every
//! `INSERT OR IGNORE` fixture `init` lays down lives here so the
//! seeding surface is one file instead of being interleaved with
//! pool/migration wiring.

use super::{DbError, DbPool};

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
pub(super) async fn seed_demo_workflow(pool: &DbPool) -> Result<(), DbError> {
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
pub(super) async fn seed_demo_canvas(pool: &DbPool) -> Result<(), DbError> {
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
pub(super) async fn seed_mcp_servers(pool: &DbPool) -> Result<(), DbError> {
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
