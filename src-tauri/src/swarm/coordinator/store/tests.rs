use super::*;
use super::cols::truncate_chars;
use crate::swarm::coordinator::job::{Job, JobState, StageResult};
use crate::swarm::coordinator::JobRegistry;
use crate::test_support::fresh_pool;

fn fixture_job(id: &str, goal: &str, created_at_ms: i64) -> Job {
    Job {
        id: id.to_string(),
        goal: goal.to_string(),
        created_at_ms,
        state: JobState::Init,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        source: Job::default_source(),
    }
}

fn fixture_stage(state: JobState, cost: f64, dur: u64) -> StageResult {
    StageResult {
        state,
        specialist_id: format!("{state:?}").to_lowercase(),
        assistant_text: format!("text-{state:?}"),
        session_id: format!("sess-{state:?}"),
        total_cost_usd: cost,
        duration_ms: dur,
        verdict: None,
        coordinator_decision: None,
    }
}

/// Migration 0006 creates the three swarm tables.
#[tokio::test]
async fn migration_0006_creates_three_tables() {
    let (pool, _dir) = fresh_pool().await;
    for name in [
        "swarm_jobs",
        "swarm_stages",
        "swarm_workspace_locks",
    ] {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type='table' AND name = ?",
        )
        .bind(name)
        .fetch_one(&pool)
        .await
        .expect("query");
        assert_eq!(count, 1, "table `{name}` missing post-migration");
    }
}

/// Driving the registry's `try_acquire_workspace` writes both a
/// job row and a workspace_lock row.
#[tokio::test]
async fn insert_job_and_lock_round_trip() {
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    let job = fixture_job("j-1", "goal one", 1000);
    reg.try_acquire_workspace("ws-1", job.clone())
        .await
        .expect("acquire");

    let row_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM swarm_jobs WHERE id = ?")
            .bind("j-1")
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(row_count, 1);
    let lock_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM swarm_workspace_locks WHERE workspace_id = ?",
    )
    .bind("ws-1")
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(lock_count, 1);

    // The persisted state matches the in-memory snapshot.
    let state_str: String =
        sqlx::query_scalar("SELECT state FROM swarm_jobs WHERE id = ?")
            .bind("j-1")
            .fetch_one(&pool)
            .await
            .expect("state");
    assert_eq!(state_str, "init");
}

/// Driving a Job through Scout/Plan/Build/Done via `update`
/// lands each intermediate state in the DB.
#[tokio::test]
async fn update_job_persists_state_transitions() {
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    reg.try_acquire_workspace("ws-2", fixture_job("j-2", "g", 0))
        .await
        .expect("acquire");

    for state in
        [JobState::Scout, JobState::Plan, JobState::Build, JobState::Done]
    {
        reg.update("j-2", |j| {
            j.state = state;
        })
        .await
        .expect("update");
        let on_disk: String = sqlx::query_scalar(
            "SELECT state FROM swarm_jobs WHERE id = ?",
        )
        .bind("j-2")
        .fetch_one(&pool)
        .await
        .expect("read state");
        assert_eq!(on_disk, state.as_db_str());
    }
    // Terminal state populated `finished_at_ms`.
    let finished: Option<i64> = sqlx::query_scalar(
        "SELECT finished_at_ms FROM swarm_jobs WHERE id = ?",
    )
    .bind("j-2")
    .fetch_one(&pool)
    .await
    .expect("read finished");
    assert!(finished.is_some(), "Done state must populate finished_at_ms");
}

/// Pushing a `StageResult` via `update` writes a `swarm_stages`
/// row at the right `idx`.
#[tokio::test]
async fn insert_stage_appends_to_job() {
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    reg.try_acquire_workspace("ws-3", fixture_job("j-3", "g", 0))
        .await
        .expect("acquire");
    reg.update("j-3", |j| {
        j.stages.push(fixture_stage(JobState::Scout, 0.01, 50));
    })
    .await
    .expect("first stage");
    reg.update("j-3", |j| {
        j.stages.push(fixture_stage(JobState::Plan, 0.02, 60));
    })
    .await
    .expect("second stage");

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM swarm_stages WHERE job_id = ?",
    )
    .bind("j-3")
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(count, 2);
    let idxs: Vec<i64> = sqlx::query_scalar(
        "SELECT idx FROM swarm_stages WHERE job_id = ? ORDER BY idx",
    )
    .bind("j-3")
    .fetch_all(&pool)
    .await
    .expect("idxs");
    assert_eq!(idxs, vec![0, 1]);
}

/// Calling `release_workspace` deletes the lock row.
#[tokio::test]
async fn release_workspace_deletes_lock_row() {
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    reg.try_acquire_workspace("ws-4", fixture_job("j-4", "g", 0))
        .await
        .expect("acquire");
    reg.release_workspace("ws-4", "j-4").await;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM swarm_workspace_locks WHERE workspace_id = ?",
    )
    .bind("ws-4")
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(count, 0);
}

/// `recover_orphans` flips non-terminal rows to Failed and
/// stamps the canonical message + finished_at_ms.
#[tokio::test]
async fn recover_orphans_flips_non_terminal_jobs_to_failed() {
    let (pool, _dir) = fresh_pool().await;
    // Seed a Scout-state orphan directly via SQL — bypasses the
    // registry so we can simulate "previous process left this".
    sqlx::query(
        "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("j-orphan")
    .bind("ws-x")
    .bind("g")
    .bind(123_i64)
    .bind("scout")
    .bind(0_i64)
    .execute(&pool)
    .await
    .expect("seed orphan");

    let result = recover_orphans(&pool, 999_999).await.expect("recover");
    assert_eq!(result.count, 1);
    let state: String = sqlx::query_scalar(
        "SELECT state FROM swarm_jobs WHERE id = ?",
    )
    .bind("j-orphan")
    .fetch_one(&pool)
    .await
    .expect("read state");
    assert_eq!(state, "failed");
    let last_err: Option<String> = sqlx::query_scalar(
        "SELECT last_error FROM swarm_jobs WHERE id = ?",
    )
    .bind("j-orphan")
    .fetch_one(&pool)
    .await
    .expect("read last_error");
    assert_eq!(last_err.as_deref(), Some("interrupted by app restart"));
    let finished: Option<i64> = sqlx::query_scalar(
        "SELECT finished_at_ms FROM swarm_jobs WHERE id = ?",
    )
    .bind("j-orphan")
    .fetch_one(&pool)
    .await
    .expect("read finished");
    assert_eq!(finished, Some(999_999));
}

/// `recover_orphans` clears every workspace_lock row.
#[tokio::test]
async fn recover_orphans_releases_workspace_locks() {
    let (pool, _dir) = fresh_pool().await;
    // Seed orphan + lock together.
    sqlx::query(
        "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("j-locky")
    .bind("ws-locky")
    .bind("g")
    .bind(0_i64)
    .bind("plan")
    .bind(0_i64)
    .execute(&pool)
    .await
    .expect("seed orphan");
    sqlx::query(
        "INSERT INTO swarm_workspace_locks (workspace_id, job_id, acquired_at_ms) \
         VALUES (?, ?, ?)",
    )
    .bind("ws-locky")
    .bind("j-locky")
    .bind(0_i64)
    .execute(&pool)
    .await
    .expect("seed lock");

    recover_orphans(&pool, 1).await.expect("recover");
    let lock_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM swarm_workspace_locks",
    )
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(lock_count, 0, "lock rows cleared by recovery");
}

/// `recover_orphans` leaves Done/Failed rows untouched.
#[tokio::test]
async fn recover_orphans_leaves_terminal_jobs_alone() {
    let (pool, _dir) = fresh_pool().await;
    sqlx::query(
        "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count, finished_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("j-done")
    .bind("ws")
    .bind("g")
    .bind(0_i64)
    .bind("done")
    .bind(0_i64)
    .bind(100_i64)
    .execute(&pool)
    .await
    .expect("seed done");
    sqlx::query(
        "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count, last_error, finished_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("j-failed")
    .bind("ws")
    .bind("g")
    .bind(0_i64)
    .bind("failed")
    .bind(0_i64)
    .bind("boom")
    .bind(101_i64)
    .execute(&pool)
    .await
    .expect("seed failed");

    let result = recover_orphans(&pool, 999).await.expect("recover");
    assert_eq!(result.count, 0, "no orphans to recover");
    // finished_at_ms unchanged for both.
    let done_finished: Option<i64> = sqlx::query_scalar(
        "SELECT finished_at_ms FROM swarm_jobs WHERE id = ?",
    )
    .bind("j-done")
    .fetch_one(&pool)
    .await
    .expect("read done");
    assert_eq!(done_finished, Some(100));
    let failed_finished: Option<i64> = sqlx::query_scalar(
        "SELECT finished_at_ms FROM swarm_jobs WHERE id = ?",
    )
    .bind("j-failed")
    .fetch_one(&pool)
    .await
    .expect("read failed");
    assert_eq!(failed_finished, Some(101));
}

/// `list_jobs(workspace_id=Some)` filters on the workspace
/// column. Seed 2×3 jobs and assert.
#[tokio::test]
async fn list_jobs_filters_by_workspace() {
    let (pool, _dir) = fresh_pool().await;
    for (id, ws, ts) in [
        ("j-a1", "ws-A", 100_i64),
        ("j-a2", "ws-A", 200_i64),
        ("j-a3", "ws-A", 300_i64),
        ("j-b1", "ws-B", 110_i64),
        ("j-b2", "ws-B", 210_i64),
        ("j-b3", "ws-B", 310_i64),
    ] {
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(ws)
        .bind("g")
        .bind(ts)
        .bind("done")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect("seed");
    }
    let a = list_jobs(&pool, Some("ws-A"), 50).await.expect("list A");
    assert_eq!(a.len(), 3);
    for s in &a {
        assert_eq!(s.workspace_id, "ws-A");
    }
    let b = list_jobs(&pool, Some("ws-B"), 50).await.expect("list B");
    assert_eq!(b.len(), 3);
    let all = list_jobs(&pool, None, 50).await.expect("list all");
    assert_eq!(all.len(), 6);
}

/// `list_jobs` truncates `goal` to 200 characters (char count,
/// not byte count) so multi-byte Turkish text never gets split.
#[tokio::test]
async fn list_jobs_truncates_goal_to_200_chars() {
    let (pool, _dir) = fresh_pool().await;
    // 500-char goal that mixes ASCII + Turkish ç so byte length
    // != char length.
    let long_goal: String = "çş".repeat(250);
    assert_eq!(long_goal.chars().count(), 500);
    sqlx::query(
        "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("j-long")
    .bind("ws")
    .bind(&long_goal)
    .bind(0_i64)
    .bind("done")
    .bind(0_i64)
    .execute(&pool)
    .await
    .expect("seed");
    let summaries = list_jobs(&pool, None, 50).await.expect("list");
    assert_eq!(summaries.len(), 1);
    assert!(
        summaries[0].goal.chars().count() <= 200,
        "goal char count: {}",
        summaries[0].goal.chars().count()
    );
    assert_eq!(summaries[0].goal.chars().count(), 200);
}

/// `list_jobs` respects the limit argument.
#[tokio::test]
async fn list_jobs_respects_limit() {
    let (pool, _dir) = fresh_pool().await;
    for i in 0..100 {
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(format!("j-{i:03}"))
        .bind("ws")
        .bind("g")
        .bind(i as i64)
        .bind("done")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect("seed");
    }
    let first = list_jobs(&pool, None, 10).await.expect("list");
    assert_eq!(first.len(), 10);
}

/// `list_jobs` orders results newest-first.
#[tokio::test]
async fn list_jobs_orders_by_created_desc() {
    let (pool, _dir) = fresh_pool().await;
    for (id, ts) in [
        ("j-old", 100_i64),
        ("j-mid", 200_i64),
        ("j-new", 300_i64),
    ] {
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind("ws")
        .bind("g")
        .bind(ts)
        .bind("done")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect("seed");
    }
    let summaries = list_jobs(&pool, None, 50).await.expect("list");
    let ids: Vec<&str> =
        summaries.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["j-new", "j-mid", "j-old"]);
}

/// `get_job_detail` returns every stage in `idx` order.
#[tokio::test]
async fn get_job_detail_returns_full_stages() {
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    reg.try_acquire_workspace("ws", fixture_job("j-d", "g", 0))
        .await
        .expect("acquire");
    for state in [JobState::Scout, JobState::Plan, JobState::Build] {
        reg.update("j-d", |j| {
            j.stages.push(fixture_stage(state, 0.01, 10));
        })
        .await
        .expect("push stage");
    }
    let detail = get_job_detail(&pool, "j-d")
        .await
        .expect("query")
        .expect("Some");
    assert_eq!(detail.stages.len(), 3);
    assert_eq!(detail.stages[0].state, JobState::Scout);
    assert_eq!(detail.stages[1].state, JobState::Plan);
    assert_eq!(detail.stages[2].state, JobState::Build);
    assert!(detail.total_cost_usd > 0.0);
}

/// Unknown ids return `Ok(None)`.
#[tokio::test]
async fn get_job_detail_unknown_returns_none() {
    let (pool, _dir) = fresh_pool().await;
    let detail =
        get_job_detail(&pool, "j-nope").await.expect("query");
    assert!(detail.is_none());
}

/// `truncate_chars` short-circuits when the input fits.
#[test]
fn truncate_chars_passthrough() {
    let s = "abc";
    assert_eq!(truncate_chars(s, 10), "abc");
}

/// `truncate_chars` cuts on character boundaries.
#[test]
fn truncate_chars_cuts_on_codepoint() {
    let s = "abçdeş";
    let t = truncate_chars(s, 3);
    assert_eq!(t.chars().count(), 3);
    assert_eq!(t, "abç");
}

/// WP-W3-12d — a Failed job with a populated `last_verdict`
/// round-trips through the registry → SQLite → store reload.
/// The Verdict must reappear with bit-for-bit fidelity (issue
/// list, severities, summary). Per-stage verdicts on Review /
/// Test stages also survive.
#[tokio::test]
async fn verdict_persists_across_app_restart() {
    use crate::swarm::coordinator::verdict::{
        Verdict, VerdictIssue, VerdictSeverity,
    };
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    reg.try_acquire_workspace(
        "ws-verdict",
        fixture_job("j-verdict", "g", 0),
    )
    .await
    .expect("acquire");

    // Push a Review stage with a populated Verdict, then mark
    // the job Failed with `last_verdict` set — mirrors the
    // FSM's `finalize_failed_with_verdict` shape.
    let approved_review = Verdict {
        approved: true,
        issues: Vec::new(),
        summary: "looks fine".to_string(),
    };
    let rejected_test = Verdict {
        approved: false,
        issues: vec![VerdictIssue {
            severity: VerdictSeverity::High,
            file: Some("tests/foo.rs".to_string()),
            line: Some(7),
            message: "test_bar fails".to_string(),
        }],
        summary: "1 failure".to_string(),
    };
    let approved_review_clone = approved_review.clone();
    let rejected_test_clone = rejected_test.clone();
    reg.update("j-verdict", |j| {
        j.stages.push(StageResult {
            state: JobState::Review,
            specialist_id: "backend-reviewer".into(),
            assistant_text: "ok".into(),
            session_id: "s-r".into(),
            total_cost_usd: 0.001,
            duration_ms: 12,
            verdict: Some(approved_review_clone),
            coordinator_decision: None,
        });
        j.state = JobState::Failed;
        j.last_verdict = Some(rejected_test_clone);
    })
    .await
    .expect("update");

    // Reload through the read path — get_job_detail must surface
    // both the per-stage verdict and the job-level last_verdict.
    let detail = get_job_detail(&pool, "j-verdict")
        .await
        .expect("query")
        .expect("Some");
    assert_eq!(detail.state, JobState::Failed);
    assert_eq!(detail.last_verdict.as_ref(), Some(&rejected_test));
    assert_eq!(detail.stages.len(), 1);
    assert_eq!(
        detail.stages[0].verdict.as_ref(),
        Some(&approved_review)
    );
    assert_eq!(detail.last_error, None);
}

/// WP-W3-12f — a Classify stage with a populated
/// `coordinator_decision` round-trips through the registry →
/// SQLite → store reload. The decision must reappear with
/// bit-for-bit fidelity (route + reasoning).
#[tokio::test]
async fn coordinator_decision_persists_across_app_restart() {
    use crate::swarm::coordinator::decision::{
        CoordinatorDecision, CoordinatorRoute,
    };
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    reg.try_acquire_workspace(
        "ws-decision",
        fixture_job("j-decision", "g", 0),
    )
    .await
    .expect("acquire");

    let decision = CoordinatorDecision {
        route: CoordinatorRoute::ResearchOnly,
        // W3-12g: scope is required on the wire shape; persisted
        // legacy rows default to Backend via serde.
        scope: crate::swarm::coordinator::decision::CoordinatorScope::Backend,
        reasoning: "explain-only goal; Scout findings cover it".into(),
    };
    let decision_clone = decision.clone();
    reg.update("j-decision", |j| {
        // First a Scout stage with no decision (canonical shape).
        j.stages.push(StageResult {
            state: JobState::Scout,
            specialist_id: "scout".into(),
            assistant_text: "scout findings".into(),
            session_id: "s-sc".into(),
            total_cost_usd: 0.001,
            duration_ms: 10,
            verdict: None,
            coordinator_decision: None,
        });
        // Then a Classify stage with the decision stamped on.
        j.stages.push(StageResult {
            state: JobState::Classify,
            specialist_id: "coordinator".into(),
            assistant_text: serde_json::to_string(&decision_clone)
                .unwrap(),
            session_id: "s-cls".into(),
            total_cost_usd: 0.001,
            duration_ms: 5,
            verdict: None,
            coordinator_decision: Some(decision_clone),
        });
        j.state = JobState::Done;
    })
    .await
    .expect("update");

    let detail = get_job_detail(&pool, "j-decision")
        .await
        .expect("query")
        .expect("Some");
    assert_eq!(detail.stages.len(), 2);
    assert!(detail.stages[0].coordinator_decision.is_none());
    assert_eq!(
        detail.stages[1].coordinator_decision.as_ref(),
        Some(&decision)
    );
    assert_eq!(detail.stages[1].state, JobState::Classify);
}

/// WP-W3-12g — `CoordinatorDecision.scope` round-trips through
/// SQLite (route + scope + reasoning all bit-for-bit). Sister
/// test to `coordinator_decision_persists_across_app_restart`
/// pinning the new `scope` field specifically — guards against
/// future serializer drift dropping the field on the wire.
#[tokio::test]
async fn coordinator_decision_round_trips_through_sqlite_with_scope() {
    use crate::swarm::coordinator::decision::{
        CoordinatorDecision, CoordinatorRoute, CoordinatorScope,
    };
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    reg.try_acquire_workspace(
        "ws-scope",
        fixture_job("j-scope", "g", 0),
    )
    .await
    .expect("acquire");

    let decision = CoordinatorDecision {
        route: CoordinatorRoute::ExecutePlan,
        scope: CoordinatorScope::Frontend,
        reasoning: "frontend goal; execute via FE chain".into(),
    };
    let decision_clone = decision.clone();
    reg.update("j-scope", |j| {
        j.stages.push(StageResult {
            state: JobState::Classify,
            specialist_id: "coordinator".into(),
            assistant_text: serde_json::to_string(&decision_clone)
                .unwrap(),
            session_id: "s-cls".into(),
            total_cost_usd: 0.001,
            duration_ms: 5,
            verdict: None,
            coordinator_decision: Some(decision_clone),
        });
        j.state = JobState::Done;
    })
    .await
    .expect("update");

    let detail = get_job_detail(&pool, "j-scope")
        .await
        .expect("query")
        .expect("Some");
    let reloaded = detail.stages[0]
        .coordinator_decision
        .as_ref()
        .expect("decision present after reload");
    // Field-by-field assertion so a regression on any of the
    // three points the failure at the right line.
    assert_eq!(reloaded.route, CoordinatorRoute::ExecutePlan);
    assert_eq!(reloaded.scope, CoordinatorScope::Frontend);
    assert_eq!(
        reloaded.reasoning,
        "frontend goal; execute via FE chain"
    );
    // Sanity: the entire struct round-trips by value too.
    assert_eq!(reloaded, &decision);
}

/// Migration 0008 adds `decision_json` to `swarm_stages`.
/// Cheap schema-pragma probe so future migration drift surfaces
/// here rather than mid-write.
#[tokio::test]
async fn migration_0008_adds_decision_column() {
    let (pool, _dir) = fresh_pool().await;
    let stage_cols: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('swarm_stages')",
    )
    .fetch_all(&pool)
    .await
    .expect("pragma swarm_stages");
    assert!(
        stage_cols.iter().any(|c| c == "decision_json"),
        "swarm_stages.decision_json missing; cols={stage_cols:?}"
    );
}

/// Migration 0007 actually adds the two new columns. Cheap
/// schema-pragma probe so future migration drift surfaces here
/// rather than mid-write.
#[tokio::test]
async fn migration_0007_adds_verdict_columns() {
    let (pool, _dir) = fresh_pool().await;
    let stage_cols: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('swarm_stages')",
    )
    .fetch_all(&pool)
    .await
    .expect("pragma swarm_stages");
    assert!(
        stage_cols.iter().any(|c| c == "verdict_json"),
        "swarm_stages.verdict_json missing; cols={stage_cols:?}"
    );
    let job_cols: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('swarm_jobs')",
    )
    .fetch_all(&pool)
    .await
    .expect("pragma swarm_jobs");
    assert!(
        job_cols.iter().any(|c| c == "last_verdict_json"),
        "swarm_jobs.last_verdict_json missing; cols={job_cols:?}"
    );
}
