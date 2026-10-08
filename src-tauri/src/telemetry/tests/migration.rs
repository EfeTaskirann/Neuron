//! Schema-level checks for the `0005` migration: the column count on
//! `runs_spans` grows by two (`exported_at` / `sampled_in`) and the
//! partial index lands in `sqlite_master` with both predicate columns.

#[tokio::test]
async fn migration_0005_adds_two_columns_to_runs_spans() {
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    // PRAGMA table_info returns one row per column. Pre-WP3-06 the
    // table had 11 columns; this WP adds `exported_at` and
    // `sampled_in` for a total of 13.
    let cols: Vec<(i64, String)> = sqlx::query_as(
        "SELECT cid, name FROM pragma_table_info('runs_spans')",
    )
    .fetch_all(&pool)
    .await
    .expect("pragma table_info");
    assert_eq!(
        cols.len(),
        13,
        "runs_spans must have 11 + 2 columns post-0005 (got {cols:?})"
    );
    let names: Vec<&str> = cols.iter().map(|(_, n)| n.as_str()).collect();
    assert!(names.contains(&"exported_at"), "exported_at column missing");
    assert!(names.contains(&"sampled_in"), "sampled_in column missing");
}

#[tokio::test]
async fn migration_0005_partial_index_lands_in_sqlite_master() {
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type='index' AND name='idx_runs_spans_export_pending'",
    )
    .fetch_optional(&pool)
    .await
    .expect("sqlite_master query");
    let sql = sql.expect("idx_runs_spans_export_pending must exist");
    // The partial-index predicate must include both columns so the
    // sweep skips sentinel and unsampled rows.
    assert!(
        sql.contains("exported_at IS NULL"),
        "predicate missing exported_at: {sql}"
    );
    assert!(
        sql.contains("sampled_in = 1"),
        "predicate missing sampled_in: {sql}"
    );
}
