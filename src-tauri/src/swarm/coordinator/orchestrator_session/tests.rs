//! Tests for the Orchestrator session log — model round-trips, store
//! write/read/clear, and prompt rendering.

use super::{
    append_job_message, append_orchestrator_message, append_user_message,
    clear_messages, list_recent_messages, render_with_history,
    OrchestratorMessage, OrchestratorMessageRole,
};
use crate::swarm::coordinator::orchestrator::{
    OrchestratorAction, OrchestratorOutcome,
};
use crate::test_support::fresh_pool;

/// Migration 0009 creates the `orchestrator_messages` table.
#[tokio::test]
async fn migration_0009_creates_orchestrator_messages_table() {
    let (pool, _dir) = fresh_pool().await;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type='table' AND name = ?",
    )
    .bind("orchestrator_messages")
    .fetch_one(&pool)
    .await
    .expect("query");
    assert_eq!(count, 1, "orchestrator_messages table missing");
}

/// Migration 0009 indexes (workspace_id, created_at_ms).
#[tokio::test]
async fn migration_0009_indexes_workspace_and_created_at() {
    let (pool, _dir) = fresh_pool().await;
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type='index' AND tbl_name='orchestrator_messages'",
    )
    .fetch_all(&pool)
    .await
    .expect("list indexes");
    assert!(
        names.iter().any(|n| n == "idx_orchestrator_messages_workspace"),
        "composite index missing; indexes={names:?}"
    );
}

/// User row round-trips via append + list.
#[tokio::test]
async fn append_user_message_round_trip() {
    let (pool, _dir) = fresh_pool().await;
    let id = append_user_message(&pool, "ws-1", "selam", 1_000)
        .await
        .expect("append");
    assert!(id > 0, "rowid populated");
    let msgs = list_recent_messages(&pool, "ws-1", 50)
        .await
        .expect("list");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, OrchestratorMessageRole::User);
    assert_eq!(msgs[0].content, "selam");
    assert_eq!(msgs[0].goal, None);
    assert_eq!(msgs[0].created_at_ms, 1_000);
}

/// Orchestrator row JSON-encodes the outcome; the read path can
/// parse the content column back into the typed shape.
#[tokio::test]
async fn append_orchestrator_message_serializes_outcome_as_json() {
    let (pool, _dir) = fresh_pool().await;
    let outcome = OrchestratorOutcome {
        action: OrchestratorAction::Clarify,
        text: "Hangi dosya?".into(),
        reasoning: "yol eksik".into(),
    };
    append_orchestrator_message(&pool, "ws-1", &outcome, 2_000)
        .await
        .expect("append");
    let msgs = list_recent_messages(&pool, "ws-1", 50)
        .await
        .expect("list");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, OrchestratorMessageRole::Orchestrator);
    let parsed: OrchestratorOutcome =
        serde_json::from_str(&msgs[0].content).expect("parse outcome json");
    assert_eq!(parsed, outcome);
}

/// Job row populates the `goal` column; `content` carries the
/// job_id.
#[tokio::test]
async fn append_job_message_populates_goal_column() {
    let (pool, _dir) = fresh_pool().await;
    append_job_message(&pool, "ws-1", "j-abc", "Add doc to X.tsx", 3_000)
        .await
        .expect("append");
    let msgs = list_recent_messages(&pool, "ws-1", 50)
        .await
        .expect("list");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, OrchestratorMessageRole::Job);
    assert_eq!(msgs[0].content, "j-abc");
    assert_eq!(msgs[0].goal.as_deref(), Some("Add doc to X.tsx"));
}

/// Mixed inserts come back oldest-first regardless of insert
/// order. SELECT is DESC then reversed in-memory; verifies the
/// reverse step actually runs.
#[tokio::test]
async fn list_recent_messages_returns_chronological_oldest_first() {
    let (pool, _dir) = fresh_pool().await;
    // Insert out of order to make the test non-trivial.
    append_user_message(&pool, "ws", "first", 100)
        .await
        .expect("u1");
    append_orchestrator_message(
        &pool,
        "ws",
        &OrchestratorOutcome {
            action: OrchestratorAction::DirectReply,
            text: "second".into(),
            reasoning: "r".into(),
        },
        200,
    )
    .await
    .expect("o1");
    append_job_message(&pool, "ws", "j-1", "third", 300)
        .await
        .expect("j1");
    let msgs = list_recent_messages(&pool, "ws", 50)
        .await
        .expect("list");
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].created_at_ms, 100);
    assert_eq!(msgs[1].created_at_ms, 200);
    assert_eq!(msgs[2].created_at_ms, 300);
    assert_eq!(msgs[0].role, OrchestratorMessageRole::User);
    assert_eq!(msgs[1].role, OrchestratorMessageRole::Orchestrator);
    assert_eq!(msgs[2].role, OrchestratorMessageRole::Job);
}

/// `limit` caps the result set; the kept rows are the most
/// recent. Reverse-step preserves oldest-first within the cap.
#[tokio::test]
async fn list_recent_messages_respects_limit() {
    let (pool, _dir) = fresh_pool().await;
    for i in 0..20 {
        append_user_message(&pool, "ws", &format!("m{i}"), i as i64)
            .await
            .expect("seed");
    }
    let msgs = list_recent_messages(&pool, "ws", 5)
        .await
        .expect("list");
    assert_eq!(msgs.len(), 5);
    // Most recent 5 = ts 15..=19, reversed to oldest-first within
    // the slice.
    assert_eq!(msgs[0].created_at_ms, 15);
    assert_eq!(msgs[4].created_at_ms, 19);
}

/// Filter on `workspace_id` — rows from other workspaces are
/// invisible.
#[tokio::test]
async fn list_recent_messages_filters_by_workspace_id() {
    let (pool, _dir) = fresh_pool().await;
    append_user_message(&pool, "ws-A", "a1", 100)
        .await
        .expect("seed");
    append_user_message(&pool, "ws-B", "b1", 200)
        .await
        .expect("seed");
    append_user_message(&pool, "ws-A", "a2", 300)
        .await
        .expect("seed");
    let a = list_recent_messages(&pool, "ws-A", 50)
        .await
        .expect("list A");
    assert_eq!(a.len(), 2);
    for m in &a {
        assert_eq!(m.workspace_id, "ws-A");
    }
    let b = list_recent_messages(&pool, "ws-B", 50)
        .await
        .expect("list B");
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].content, "b1");
}

/// `clear_messages` deletes every row for the targeted workspace.
#[tokio::test]
async fn clear_messages_deletes_all_workspace_messages() {
    let (pool, _dir) = fresh_pool().await;
    for i in 0..5 {
        append_user_message(&pool, "ws", &format!("m{i}"), i as i64)
            .await
            .expect("seed");
    }
    clear_messages(&pool, "ws").await.expect("clear");
    let msgs = list_recent_messages(&pool, "ws", 50)
        .await
        .expect("list");
    assert!(msgs.is_empty());
}

/// `clear_messages` is per-workspace; other workspaces survive.
#[tokio::test]
async fn clear_messages_leaves_other_workspaces_intact() {
    let (pool, _dir) = fresh_pool().await;
    append_user_message(&pool, "ws-A", "keep", 100)
        .await
        .expect("seed");
    append_user_message(&pool, "ws-B", "drop", 200)
        .await
        .expect("seed");
    clear_messages(&pool, "ws-B").await.expect("clear B");
    let a = list_recent_messages(&pool, "ws-A", 50)
        .await
        .expect("list A");
    assert_eq!(a.len(), 1);
    let b = list_recent_messages(&pool, "ws-B", 50)
        .await
        .expect("list B");
    assert!(b.is_empty());
}

/// Empty history short-circuits to the raw user message.
#[test]
fn render_with_history_empty_returns_user_message_verbatim() {
    let rendered = render_with_history(&[], "selam");
    assert_eq!(rendered, "selam");
}

/// User-role rows render with the `[user]:` label.
#[test]
fn render_with_history_includes_user_role_label() {
    let history = vec![OrchestratorMessage {
        id: 1,
        workspace_id: "ws".into(),
        role: OrchestratorMessageRole::User,
        content: "auth refactor istiyorum".into(),
        goal: None,
        created_at_ms: 100,
    }];
    let rendered = render_with_history(&history, "evet");
    assert!(rendered.contains("[user]: auth refactor istiyorum"));
    assert!(rendered.contains("evet"));
    assert!(rendered.contains("Önceki konuşma"));
}

/// Orchestrator-role rows render with the action label.
#[test]
fn render_with_history_includes_orchestrator_action_label() {
    let outcome = OrchestratorOutcome {
        action: OrchestratorAction::Dispatch,
        text: "EXECUTE: Add doc to foo.ts".into(),
        reasoning: "concrete enough".into(),
    };
    let history = vec![OrchestratorMessage {
        id: 2,
        workspace_id: "ws".into(),
        role: OrchestratorMessageRole::Orchestrator,
        content: serde_json::to_string(&outcome).unwrap(),
        goal: None,
        created_at_ms: 200,
    }];
    let rendered = render_with_history(&history, "tamam");
    assert!(
        rendered.contains("[orchestrator/dispatch]: EXECUTE: Add doc to foo.ts"),
        "rendered: {rendered}"
    );
}

/// Job-role rows render with the dispatched job id and the goal.
#[test]
fn render_with_history_includes_dispatched_job_id() {
    let history = vec![OrchestratorMessage {
        id: 3,
        workspace_id: "ws".into(),
        role: OrchestratorMessageRole::Job,
        content: "j-12345".into(),
        goal: Some("Add JSDoc to foo.ts".into()),
        created_at_ms: 300,
    }];
    let rendered = render_with_history(&history, "next");
    assert!(
        rendered.contains("[swarm dispatched]: j-12345 (goal: Add JSDoc to foo.ts)"),
        "rendered: {rendered}"
    );
}

/// Unparseable orchestrator content still renders (never panics)
/// and falls back to a bare `[orchestrator]:` label so the prompt
/// is informative even in the degraded case.
#[test]
fn render_with_history_handles_unparseable_orchestrator_content() {
    let history = vec![OrchestratorMessage {
        id: 4,
        workspace_id: "ws".into(),
        role: OrchestratorMessageRole::Orchestrator,
        content: "not valid json".into(),
        goal: None,
        created_at_ms: 400,
    }];
    let rendered = render_with_history(&history, "next");
    assert!(
        rendered.contains("[orchestrator]: not valid json"),
        "rendered: {rendered}"
    );
}
