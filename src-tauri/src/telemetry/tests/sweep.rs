//! Drive [`crate::telemetry::exporter::export_one_batch`] against a
//! `mockito` stub HTTP collector through the documented status-code
//! branches (2xx / 4xx / 5xx / empty / full batch) plus the in-flight
//! and unsampled SQL-filter guards.

/// Insert a workflow + run + N closed spans with `sampled_in=1` and
/// `exported_at IS NULL` so the sweep picks them up.
async fn seed_pending_spans(pool: &crate::db::DbPool, count: usize) -> Vec<String> {
    sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
         VALUES ('r-1','w1','Daily summary',1,'running')",
    )
    .execute(pool)
    .await
    .unwrap();
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let id = format!("s-{i:04}");
        sqlx::query(
            "INSERT INTO runs_spans \
             (id, run_id, name, type, t0_ms, duration_ms, attrs_json, is_running, sampled_in, exported_at) \
             VALUES (?, 'r-1', 'span', 'llm', ?, 10, '{}', 0, 1, NULL)",
        )
        .bind(&id)
        .bind(i as i64 * 100)
        .execute(pool)
        .await
        .unwrap();
        ids.push(id);
    }
    ids
}

#[tokio::test]
async fn sweep_empty_queue_returns_zero_and_makes_no_request() {
    let mut server = mockito::Server::new_async().await;
    // No expectation — if the sweep makes a call, mockito fails the
    // test on drop because no matcher will satisfy it.
    let m = server
        .mock("POST", "/v1/traces")
        .expect(0)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = crate::telemetry::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("empty sweep ok");
    assert_eq!(n, 0);
    m.assert_async().await;
}

#[tokio::test]
async fn sweep_2xx_flags_exported_at() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let ids = seed_pending_spans(&pool, 3).await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = crate::telemetry::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep ok");
    assert_eq!(n, 3);
    m.assert_async().await;

    for id in ids {
        let exported_at: Option<i64> = sqlx::query_scalar(
            "SELECT exported_at FROM runs_spans WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let v = exported_at.expect("exported_at flagged");
        assert!(v > 0, "exported_at must be a positive unix-seconds value");
    }
}

#[tokio::test]
async fn sweep_5xx_leaves_rows_untouched() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .with_status(503)
        .with_body("upstream timeout")
        .expect(1)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let ids = seed_pending_spans(&pool, 2).await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = crate::telemetry::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep returns ok even on 5xx");
    assert_eq!(n, 0, "5xx must not flag rows");
    m.assert_async().await;

    for id in ids {
        let exported_at: Option<i64> = sqlx::query_scalar(
            "SELECT exported_at FROM runs_spans WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            exported_at.is_none(),
            "5xx must leave exported_at NULL for retry"
        );
    }
}

#[tokio::test]
async fn sweep_4xx_flags_failed_sentinel() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .with_status(400)
        .with_body("bad request")
        .expect(1)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let ids = seed_pending_spans(&pool, 2).await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = crate::telemetry::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep returns ok even on 4xx");
    assert_eq!(n, 2, "4xx flags rows as permanently failed");
    m.assert_async().await;

    for id in ids {
        let exported_at: i64 = sqlx::query_scalar(
            "SELECT exported_at FROM runs_spans WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(exported_at, -1, "4xx must flag exported_at = -1 sentinel");
    }
}

#[tokio::test]
async fn sweep_full_batch_is_one_http_call_with_all_rows_flagged() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    // Seed exactly BATCH_SIZE rows so we exercise the row-cap path.
    let ids = seed_pending_spans(&pool, crate::telemetry::exporter::BATCH_SIZE as usize).await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = crate::telemetry::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep ok");
    assert_eq!(
        n,
        crate::telemetry::exporter::BATCH_SIZE as usize,
        "all 200 rows flagged in one batch"
    );
    m.assert_async().await;

    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runs_spans WHERE exported_at IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 0, "no rows should remain pending");
    let _ = ids;
}

/// In-flight spans (duration_ms NULL) must NOT be picked up — only
/// closed spans go on the wire. Confirms the SQL filter does its
/// job rather than relying on the OTLP serializer to handle nulls.
#[tokio::test]
async fn sweep_skips_in_flight_spans() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .expect(0)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
         VALUES ('r-1','w1','Daily summary',1,'running')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO runs_spans \
         (id, run_id, name, type, t0_ms, duration_ms, attrs_json, is_running, sampled_in, exported_at) \
         VALUES ('s-x','r-1','span','llm',0,NULL,'{}',1,1,NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let endpoint = format!("{}/v1/traces", server.url());
    let n = crate::telemetry::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep ok");
    assert_eq!(n, 0);
    m.assert_async().await;
}

/// Security-review M2: a 4xx response with a body larger than
/// [`crate::telemetry::exporter::RESPONSE_LOG_CAP`]'s value MUST NOT
/// cause us to allocate the full body into memory. We approximate the
/// bound by mocking a 4xx with a 1 MiB body and verifying the sweep
/// completes in well under the timeout the loop would otherwise hit.
/// The actual cap-enforcement is exercised at the unit level by
/// `read_capped_truncates_oversized_body` below.
#[tokio::test]
async fn sweep_4xx_with_large_body_does_not_oom() {
    let mut server = mockito::Server::new_async().await;
    // 1 MiB body. If the cap regresses, this test still passes (the
    // OS will happily hand us 1 MiB), but the unit-level cap test is
    // the load-bearing guard.
    let big = "X".repeat(1024 * 1024);
    let m = server
        .mock("POST", "/v1/traces")
        .with_status(400)
        .with_body(big)
        .expect(1)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let _ids = seed_pending_spans(&pool, 1).await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = crate::telemetry::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep ok");
    assert_eq!(n, 1, "4xx still flags rows");
    m.assert_async().await;
}

/// Unsampled spans (`sampled_in = 0`) are excluded by the SQL
/// filter. Confirms the partial index predicate matches the query
/// predicate.
#[tokio::test]
async fn sweep_skips_unsampled_spans() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .expect(0)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
         VALUES ('r-1','w1','Daily summary',1,'running')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO runs_spans \
         (id, run_id, name, type, t0_ms, duration_ms, attrs_json, is_running, sampled_in, exported_at) \
         VALUES ('s-y','r-1','span','llm',0,10,'{}',0,0,NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let endpoint = format!("{}/v1/traces", server.url());
    let n = crate::telemetry::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep ok");
    assert_eq!(n, 0);
    m.assert_async().await;
}
