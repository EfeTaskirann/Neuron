//! Migration 0010 (`mailbox.kind` / `parent_id` / `payload_json`)
//! column round-trip.

use crate::test_support::mock_app_with_pool;

/// Acceptance: migration 0010 lands the three new columns with
/// defaults; existing-row backfill works; new rows can carry
/// non-default values.
#[tokio::test]
async fn migration_0010_round_trip() {
    let (_, pool, _dir) = mock_app_with_pool().await;

    // Insert a legacy-shape row (no kind / parent_id /
    // payload_json supplied). Defaults must apply.
    sqlx::query(
        "INSERT INTO mailbox (ts, from_pane, to_pane, type, summary) \
         VALUES (100, 'pane:p1', 'pane:p2', 'task:done', 'legacy')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (kind, parent_id, payload_json): (String, Option<i64>, String) =
        sqlx::query_as(
            "SELECT kind, parent_id, payload_json FROM mailbox WHERE summary='legacy'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kind, "note");
    assert_eq!(parent_id, None);
    assert_eq!(payload_json, "{}");

    // Insert a new-shape row with non-default values.
    sqlx::query(
        "INSERT INTO mailbox \
           (ts, from_pane, to_pane, type, summary, kind, parent_id, payload_json) \
         VALUES (200, 'agent:scout', 'agent:planner', 'task_dispatch', \
                 'dispatched', 'task_dispatch', 1, '{\"kind\":\"task_dispatch\",\"job_id\":\"j-1\",\"target\":\"agent:scout\",\"prompt\":\"go\",\"with_help_loop\":true}')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (kind2, parent2, payload2): (String, Option<i64>, String) =
        sqlx::query_as(
            "SELECT kind, parent_id, payload_json FROM mailbox WHERE summary='dispatched'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kind2, "task_dispatch");
    assert_eq!(parent2, Some(1));
    assert!(payload2.contains("\"job_id\":\"j-1\""));
}
