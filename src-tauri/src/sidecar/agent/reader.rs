//! Stdout reader → DB writer + Tauri event emitter.
//!
//! `read_loop` drains framed JSON from the child's stdout, decodes each
//! frame into a `SidecarEvent`, and dispatches it through `handle_event`
//! into the SQLite write helpers (`insert_span` / `update_span` /
//! `finalise_run`) and the `runs:{id}:span` Tauri event emitter.

use tauri::{AppHandle, Emitter, Runtime};
use tokio::io::BufReader;
use tokio::process::ChildStdout;

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::sidecar::framing::{read_frame, Frame};

use super::wire::{RunSpanPayload, SerializableWireSpan, SidecarEvent, WireSpan};

pub(super) async fn read_loop<R: Runtime>(
    stdout: ChildStdout,
    pool: DbPool,
    app: AppHandle<R>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = %e, "sidecar frame error");
                break;
            }
        };

        let body = match frame {
            Frame::Body(b) => b,
            Frame::Eof => {
                tracing::info!("sidecar stdout closed; read loop exiting");
                break;
            }
        };

        let event: SidecarEvent = match serde_json::from_slice(&body) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    body = %String::from_utf8_lossy(&body),
                    "sidecar frame decode error"
                );
                continue;
            }
        };

        if let Err(e) = handle_event(event, &pool, &app).await {
            tracing::error!(error = %e, "sidecar handle_event failed");
        }
    }
}

async fn handle_event<R: Runtime>(
    event: SidecarEvent,
    pool: &DbPool,
    app: &AppHandle<R>,
) -> Result<(), AppError> {
    match event {
        SidecarEvent::Ready => {
            // Logged for now; Week 3 surfaces this as a "Starting
            // agent" → "Ready" pill in the UI.
            tracing::info!("LangGraph agent runtime ready");
        }
        SidecarEvent::Error { message } => {
            tracing::warn!(
                message = %message.unwrap_or_else(|| "<no message>".into()),
                "sidecar reported non-fatal error"
            );
        }
        SidecarEvent::SpanCreated { run_id, span } => {
            insert_span(pool, &span).await?;
            emit_span_event(app, &run_id, "created", &span)?;
        }
        SidecarEvent::SpanUpdated { run_id, span } => {
            update_span(pool, &span).await?;
            emit_span_event(app, &run_id, "updated", &span)?;
        }
        SidecarEvent::SpanClosed { run_id, span } => {
            update_span(pool, &span).await?;
            // WP-W2-07: aggregates roll up on each span close so the
            // inspector's run-level token/cost totals advance as work
            // completes, not just at run finalisation.
            crate::commands::util::update_run_aggregates(pool, &run_id).await?;
            emit_span_event(app, &run_id, "closed", &span)?;
        }
        SidecarEvent::RunCompleted { run_id, status, error } => {
            finalise_run(pool, &run_id, &status).await?;
            // No event — the frontend's `runs:get` re-read on
            // `runs:{id}:span(closed)` already covers the UI update.
            // Logging the optional error helps dev-time debugging.
            if let Some(msg) = error {
                tracing::info!(
                    run_id = %run_id,
                    status = %status,
                    error = %msg,
                    "run completed"
                );
            }
        }
    }
    Ok(())
}

pub(super) async fn insert_span(pool: &DbPool, span: &WireSpan) -> Result<(), AppError> {
    let is_running = if span.is_running { 1_i64 } else { 0_i64 };
    // WP-W3-06: sampling decision is per-span and made at insert
    // time, not at export time, so the export sweep can rely on a
    // simple `WHERE sampled_in = 1` filter and the partial index
    // `idx_runs_spans_export_pending` does its job. The default is
    // 1 (always include) when `NEURON_OTEL_SAMPLING_RATIO` is unset.
    let sampled_in = if crate::telemetry::sampling::sampled_in() {
        1_i64
    } else {
        0_i64
    };
    sqlx::query(
        "INSERT INTO runs_spans \
         (id, run_id, parent_span_id, name, type, t0_ms, duration_ms, attrs_json, prompt, response, is_running, sampled_in) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&span.id)
    .bind(&span.run_id)
    .bind(span.parent_span_id.as_deref())
    .bind(&span.name)
    .bind(&span.span_type)
    .bind(span.t0_ms)
    .bind(span.duration_ms)
    .bind(&span.attrs_json)
    .bind(span.prompt.as_deref())
    .bind(span.response.as_deref())
    .bind(is_running)
    .bind(sampled_in)
    .execute(pool)
    .await?;
    Ok(())
}

pub(super) async fn update_span(pool: &DbPool, span: &WireSpan) -> Result<(), AppError> {
    let is_running = if span.is_running { 1_i64 } else { 0_i64 };
    sqlx::query(
        "UPDATE runs_spans SET \
            duration_ms = ?, \
            attrs_json  = ?, \
            prompt      = COALESCE(?, prompt), \
            response    = COALESCE(?, response), \
            is_running  = ? \
         WHERE id = ?",
    )
    .bind(span.duration_ms)
    .bind(&span.attrs_json)
    .bind(span.prompt.as_deref())
    .bind(span.response.as_deref())
    .bind(is_running)
    .bind(&span.id)
    .execute(pool)
    .await?;
    Ok(())
}

pub(super) async fn finalise_run(pool: &DbPool, run_id: &str, status: &str) -> Result<(), AppError> {
    // The `status` column is CHECK-constrained at SQL to one of
    // `running`, `success`, `error`. Translate any unknown sidecar
    // status into `error` so we do not violate the constraint.
    let safe_status = match status {
        "success" | "error" | "running" => status,
        _ => "error",
    };
    // Guard against ezme of a user-driven `runs:cancel` (which sets
    // status='error' before the sidecar finishes). Only finalise rows
    // still observed as `running`; if the user cancelled mid-flight
    // their `error` flag survives the late-arriving `RunCompleted`.
    sqlx::query(
        "UPDATE runs SET \
            status      = ?, \
            duration_ms = COALESCE(duration_ms, (CAST(strftime('%s','now') AS INTEGER) - started_at) * 1000) \
         WHERE id = ? AND status = 'running'",
    )
    .bind(safe_status)
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

fn emit_span_event<R: Runtime>(
    app: &AppHandle<R>,
    run_id: &str,
    kind: &str,
    span: &WireSpan,
) -> Result<(), AppError> {
    // Frontend subscribes to `runs:{id}:span` per WP-W2-08; the
    // payload's discriminant is `kind`, so a single event name covers
    // all three lifecycle stages without forcing the UI to subscribe
    // three separate channels. The wire-name helper lives in
    // `crate::events` (ADR-0006 § "Wire-format substitution").
    let event_name = events::run_span(run_id);
    // Re-encode the span as a JSON Value so the payload is the exact
    // shape the sidecar sent (post-frontend rename).
    let span_value = serde_json::to_value(SerializableWireSpan(span))?;
    let payload = RunSpanPayload {
        kind,
        span: &span_value,
    };
    app.emit(&event_name, &payload)?;
    Ok(())
}
