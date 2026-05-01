//! WP-W3-06 — periodic OTLP export sweep.
//!
//! Reads at most [`BATCH_SIZE`] pending rows from `runs_spans`,
//! builds an OTLP envelope, POSTs it to the configured endpoint,
//! and updates the row bookkeeping based on the HTTP response:
//!
//! - 2xx → flag `exported_at = strftime('%s','now')`
//! - 4xx → flag `exported_at = -1` (permanent-failure sentinel; the
//!   collector rejected the payload, retrying would just bounce)
//! - 5xx / transport error → leave rows untouched so the next loop
//!   iteration picks them up again
//!
//! The partial index `idx_runs_spans_export_pending` predicate is
//! `WHERE exported_at IS NULL AND sampled_in = 1`, so both sentinel
//! `-1` rows and unsampled rows are naturally skipped on subsequent
//! sweeps.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;
use sqlx::Row;

use crate::db::DbPool;
use crate::error::AppError;
use crate::telemetry::otlp::{build_envelope, StoredSpan};

/// How many rows per batch. 200 is well below SQLite's IN-clause
/// expansion limit (default 999) and keeps the OTLP envelope small
/// enough to fit comfortably under most collector POST-size caps
/// (~few MB given 1 KiB attribute caps × 200 spans).
pub const BATCH_SIZE: i64 = 200;

/// Sweep interval. 30 s strikes a balance between "near-real-time"
/// observability (most runs finish within tens of seconds) and not
/// hammering the collector for an empty queue.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// HTTP timeout for one POST. The collector should ack quickly; if
/// it doesn't we want to free the loop to retry rather than wedge.
pub const POST_TIMEOUT: Duration = Duration::from_secs(10);

/// Sentinel value written to `exported_at` when the collector
/// returned a 4xx response. Rows in this state are excluded from
/// future sweeps because the partial index predicate is
/// `exported_at IS NULL`.
const EXPORT_FAILED_SENTINEL: i64 = -1;

/// Lazily-initialised singleton `reqwest::Client`. Re-using one
/// `Client` across sweeps lets the connection pool keep warm
/// connections to the collector. Per WP §"HTTP client".
fn http_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(POST_TIMEOUT)
            .build()
            .expect("reqwest client build")
    })
}

/// Long-running sweep loop. Spawned by `lib.rs::setup` iff the
/// `NEURON_OTEL_ENDPOINT` env var resolved to a non-empty string.
pub async fn start_export_loop(pool: DbPool, endpoint: String) {
    tracing::info!(endpoint = %endpoint, "OTLP export loop started");
    loop {
        match export_one_batch(&pool, &endpoint).await {
            Ok(0) => {
                // Empty queue — quiet path, intentionally no log so a
                // healthy idle app doesn't spam tracing.
            }
            Ok(n) => {
                tracing::debug!(exported = n, "OTLP batch sent");
            }
            Err(e) => {
                tracing::warn!(error = %e, "OTLP export failed");
            }
        }
        tokio::time::sleep(SWEEP_INTERVAL).await;
    }
}

/// One pass: fetch up to [`BATCH_SIZE`] pending rows, POST them to
/// `endpoint`, update bookkeeping. Returns the number of rows
/// flagged (2xx ⇒ batch size, 4xx ⇒ batch size, 5xx ⇒ 0).
pub async fn export_one_batch(
    pool: &DbPool,
    endpoint: &str,
) -> Result<usize, AppError> {
    let spans = fetch_pending(pool).await?;
    if spans.is_empty() {
        return Ok(0);
    }

    let (envelope, ids) = build_envelope(&spans)?;

    let resp = match http_client()
        .post(endpoint)
        .json(&envelope)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // Transport error (DNS, connect, TLS, body, timeout). Per
            // WP §"Error & retry semantics" we leave rows untouched so
            // the next iteration picks them up.
            return Err(AppError::Internal(format!("OTLP transport: {e}")));
        }
    };

    let status = resp.status();
    if status.is_success() {
        flag_exported(pool, &ids).await?;
        Ok(ids.len())
    } else if status.is_client_error() {
        // Permanent failure — collector rejected the payload. Mark
        // the rows so we don't keep retrying a bad envelope every
        // 30 seconds for the rest of the app's lifetime.
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(
            status = %status,
            body = %body,
            count = ids.len(),
            "OTLP collector returned 4xx; flagging rows as permanently failed"
        );
        flag_failed(pool, &ids).await?;
        Ok(ids.len())
    } else {
        // 5xx — transient. Leave the rows alone.
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(
            status = %status,
            body = %body,
            "OTLP collector returned 5xx; leaving rows un-flagged for retry"
        );
        Ok(0)
    }
}

/// SELECT up to [`BATCH_SIZE`] rows that:
/// - are sampled in (`sampled_in = 1`)
/// - have not been exported (`exported_at IS NULL`)
/// - are closed (`duration_ms IS NOT NULL`) — in-flight spans are
///   intentionally not exported until their final shape is known.
async fn fetch_pending(pool: &DbPool) -> Result<Vec<StoredSpan>, AppError> {
    let rows = sqlx::query(
        "SELECT id, run_id, parent_span_id, name, t0_ms, duration_ms, \
                attrs_json, prompt, response \
         FROM runs_spans \
         WHERE sampled_in = 1 AND exported_at IS NULL AND duration_ms IS NOT NULL \
         ORDER BY t0_ms \
         LIMIT ?",
    )
    .bind(BATCH_SIZE)
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(StoredSpan {
            id: row.try_get("id")?,
            run_id: row.try_get("run_id")?,
            parent_span_id: row.try_get("parent_span_id")?,
            name: row.try_get("name")?,
            t0_ms: row.try_get("t0_ms")?,
            duration_ms: row.try_get("duration_ms")?,
            attrs_json: row.try_get("attrs_json")?,
            prompt: row.try_get("prompt")?,
            response: row.try_get("response")?,
        });
    }
    Ok(out)
}

/// Flip `exported_at = strftime('%s','now')` on a batch. The IN
/// clause is built dynamically because sqlx doesn't expand `Vec<&str>`
/// bindings into a tuple; with batches capped at 200 well under the
/// 999-default IN-list limit this is safe.
async fn flag_exported(pool: &DbPool, ids: &[String]) -> Result<(), AppError> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = std::iter::repeat("?")
        .take(ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "UPDATE runs_spans \
         SET exported_at = CAST(strftime('%s','now') AS INTEGER) \
         WHERE id IN ({placeholders})"
    );
    let mut q = sqlx::query(&sql);
    for id in ids {
        q = q.bind(id);
    }
    q.execute(pool).await?;
    Ok(())
}

/// Flip `exported_at = -1` (sentinel) on a batch the collector
/// 4xx-rejected.
async fn flag_failed(pool: &DbPool, ids: &[String]) -> Result<(), AppError> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = std::iter::repeat("?")
        .take(ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "UPDATE runs_spans \
         SET exported_at = ? \
         WHERE id IN ({placeholders})"
    );
    let mut q = sqlx::query(&sql).bind(EXPORT_FAILED_SENTINEL);
    for id in ids {
        q = q.bind(id);
    }
    q.execute(pool).await?;
    Ok(())
}
