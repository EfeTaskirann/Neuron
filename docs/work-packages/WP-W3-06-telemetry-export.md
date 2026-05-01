---
id: WP-W3-06
title: Telemetry export (OTel collector + sampling)
owner: TBD
status: not-started
depends-on: []
acceptance-gate: "Background sweep exports unexported `runs_spans` rows as OTLP/JSON to a configurable endpoint; sampling decision applied at insert time; failed POSTs leave rows un-flagged for retry."
---

## Goal

Honor `WP-W2-07 §"Out of scope"` ("OTel collector export — Week 3")
by adding a background sweep that exports stored spans from
`runs_spans` to a configurable OTLP endpoint, plus a sampling
decision applied at insert time so high-volume runs do not flood
the collector.

This WP is **independent of WP-W3-01** for parallel sub-agent
dispatch:

- The OTLP endpoint and sampling ratio are sourced from
  environment variables (`NEURON_OTEL_ENDPOINT`,
  `NEURON_OTEL_SAMPLING_RATIO`) in this WP.
- Once WP-W3-01 ships the `settings` table, a follow-up sub-task
  (≤30-line diff) wires `settings:get('otel.endpoint')` /
  `settings:get('otel.sampling.ratio')` into the same module.
  That follow-up lands as a separate commit and is NOT this WP's
  acceptance.

The "trim spans older than N days" sweep is a separate concern,
deferred to a follow-up. This WP only adds export + sampling.

## Scope

### 1. Migration `0005_span_export.sql`

```sql
ALTER TABLE runs_spans ADD COLUMN exported_at INTEGER NULL;
ALTER TABLE runs_spans ADD COLUMN sampled_in INTEGER NOT NULL DEFAULT 1;

CREATE INDEX idx_runs_spans_export_pending
  ON runs_spans (exported_at)
  WHERE exported_at IS NULL AND sampled_in = 1;
```

Index rationale: the sweep query is
`WHERE sampled_in = 1 AND exported_at IS NULL`. A partial index
on the predicate keeps the scan tight even when `runs_spans`
grows into millions of rows.

Update the `migrations_are_idempotent` test's expected count
from 4 → 5. (Or 3 → 4 if WP-W3-01 has not landed yet — verify
the current count at sub-agent time.)

### 2. Sampling at insert

`sidecar/agent.rs::insert_span` decides per-span whether to
include the row in export. Cheap path:

```rust
fn sampling_ratio() -> f64 {
    static RATIO: OnceLock<f64> = OnceLock::new();
    *RATIO.get_or_init(|| {
        std::env::var("NEURON_OTEL_SAMPLING_RATIO")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|r| (0.0..=1.0).contains(r))
            .unwrap_or(1.0)
    })
}

fn sampled_in() -> bool {
    let ratio = sampling_ratio();
    if ratio >= 1.0 { return true; }
    if ratio <= 0.0 { return false; }
    rand::random::<f64>() < ratio
}
```

Add `rand = "0.8"` to `Cargo.toml` if not already present (it
isn't as of WP-W2-08).

Sampling decision is **per-span** in this WP, NOT per-run.
Per-run sampling (entire run included or excluded) is a future
refinement — would require keeping the decision keyed by
`run_id` for the lifetime of the run, which is sidecar-protocol
work, not export-sweep work.

### 3. OTLP JSON wire shape

New module `src-tauri/src/telemetry/`:

```
src-tauri/src/telemetry/
├── mod.rs            # entry: start_export_loop()
├── otlp.rs           # OTLP JSON serializer (manual, no SDK)
└── exporter.rs       # the periodic sweep task
```

Per the OTLP HTTP/JSON spec (v1.3), the export request body is:

```jsonc
{
  "resourceSpans": [{
    "resource": {
      "attributes": [
        { "key": "service.name", "value": { "stringValue": "neuron" } }
      ]
    },
    "scopeSpans": [{
      "scope": { "name": "neuron-agent-runtime" },
      "spans": [
        {
          "traceId": "...",
          "spanId": "...",
          "parentSpanId": "...",
          "name": "...",
          "kind": 1,
          "startTimeUnixNano": "...",
          "endTimeUnixNano": "...",
          "attributes": [...],
          "status": { "code": 1 }
        }
      ]
    }]
  }]
}
```

Translation rules from `runs_spans` row to OTLP span:

| OTLP field | Source |
|---|---|
| `traceId` | `run_id` padded/hashed to 16 bytes hex (deterministic; same run_id → same trace_id across exports) |
| `spanId` | `id` (the span id) padded/hashed to 8 bytes hex |
| `parentSpanId` | `parent_span_id` if non-null (same hash) |
| `name` | `name` |
| `kind` | always `1` (INTERNAL) for now — distinction between agent-internal and tool calls is a future refinement |
| `startTimeUnixNano` | `t0_ms * 1_000_000` |
| `endTimeUnixNano` | `(t0_ms + duration_ms) * 1_000_000` if `duration_ms IS NOT NULL`, else NULL (open span — typically excluded from export until closed) |
| `attributes` | `attrs_json` parsed and converted to `[{key, value:{stringValue|intValue|doubleValue|boolValue}}]` |
| `status.code` | `1` (OK) if span has no error attr, else `2` (ERROR) |

`prompt` and `response` columns become attributes
(`gen_ai.prompt`, `gen_ai.completion`) per the OTel GenAI
semantic conventions, but truncated to 1 KiB each so collectors
don't reject the request.

### 4. Export sweep task

```rust
// telemetry/exporter.rs
pub async fn start_export_loop(pool: DbPool, endpoint: String) {
    let interval = Duration::from_secs(30);
    loop {
        match export_one_batch(&pool, &endpoint).await {
            Ok(0) => {} // nothing to export
            Ok(n) => tracing::debug!(exported = n, "OTLP batch sent"),
            Err(e) => tracing::warn!(error = %e, "OTLP export failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

async fn export_one_batch(pool: &DbPool, endpoint: &str) -> Result<usize, AppError> {
    // 1. SELECT up to N (e.g., 200) closed spans where
    //    sampled_in=1 AND exported_at IS NULL
    //    AND duration_ms IS NOT NULL
    //    ORDER BY t0_ms LIMIT 200
    // 2. Build OTLP envelope, POST to endpoint as application/json
    // 3. On 2xx: UPDATE runs_spans SET exported_at = strftime('%s','now') WHERE id IN (...)
    // 4. On non-2xx or transport error: leave rows untouched (retried next loop)
}
```

Wired from `lib.rs::run().setup(...)`:

```rust
if let Ok(endpoint) = std::env::var("NEURON_OTEL_ENDPOINT") {
    if !endpoint.trim().is_empty() {
        let pool_clone = pool.clone();
        tauri::async_runtime::spawn(async move {
            crate::telemetry::start_export_loop(pool_clone, endpoint).await;
        });
    }
}
```

If `NEURON_OTEL_ENDPOINT` is unset or empty, the loop never
starts — the app runs identically to today, exporter is a
no-op. This is the safe default for users without a collector.

### 5. HTTP client

Add `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }`
to `src-tauri/Cargo.toml`. `rustls-tls` keeps the dep tree off
OpenSSL system libs — relevant for the upcoming WP-W3-10
self-contained bundle.

Single `Client` instance reused across export iterations
(static `OnceLock<Client>`).

POST timeout: 10s. Failures are silent in steady-state (logged
at WARN); the sweep retries on next interval.

### 6. Error & retry semantics

- **2xx response**: spans flagged `exported_at`, batch done.
- **4xx response**: log at ERROR with response body
  (truncated to 512 chars). Do NOT retry the same batch — a
  4xx means the spans are malformed; if we retry forever we
  block the queue. Mark them with `exported_at = -1` (sentinel
  for "permanently failed") so the next sweep skips them.
- **5xx response**: leave rows untouched, retry next loop.
- **Connection error / timeout**: same as 5xx — leave untouched.

Add `idx_runs_spans_export_pending` to filter out the `-1`
sentinel via `WHERE exported_at IS NULL` (the original
predicate). Do NOT update the index to include `-1` — the
partial index naturally skips them.

### 7. Tests

- OTLP JSON shape round-trip: serialize one synthetic span row,
  assert against a checked-in `expected.json` fixture under
  `src-tauri/src/telemetry/tests/fixtures/`. Catches
  silent OTLP-spec drift.
- Sampling: `NEURON_OTEL_SAMPLING_RATIO=1.0` always passes;
  `=0.0` always fails; `=0.5` passes ~50% over 1000 trials
  (use a seeded PRNG so the test is deterministic).
- Export sweep against a stub HTTP server (`mockito` or
  `wiremock` crate as `[dev-dependencies]`):
  - 2xx → spans flagged `exported_at IS NOT NULL`
  - 5xx → spans untouched
  - 4xx → spans flagged `exported_at = -1`
  - empty queue → loop returns 0, no HTTP call
  - 200-row batch → exactly one HTTP call, all rows flagged
- Migration test: column count assertion for `runs_spans` grows
  by 2 (`exported_at`, `sampled_in`); the partial index exists
  in `sqlite_master`.

Target test delta: +10 to +14 unit tests.

### 8. Bindings + dev console smoke

No new Tauri commands in this WP — the export loop is
entirely backend. `bindings.ts` should NOT change. Run
`pnpm gen:bindings:check` to verify.

Manual smoke:

```bash
# 1. Start a local OTLP collector that prints to stdout
docker run --rm -p 4318:4318 \
  otel/opentelemetry-collector-contrib:latest \
  --config /etc/otelcol-contrib/config.yaml

# 2. Run Neuron with the env var set
NEURON_OTEL_ENDPOINT=http://localhost:4318/v1/traces \
NEURON_OTEL_SAMPLING_RATIO=1.0 \
pnpm tauri dev

# 3. Trigger a run from the canvas, wait ~30s, verify the
#    collector logs received N spans matching the run.
```

## Out of scope

- ❌ Per-run (rather than per-span) sampling decision
- ❌ Trim sweep (delete spans older than N days)
- ❌ Reading endpoint / ratio from `settings` table — env var
  only in this WP; settings integration is a follow-up commit
  after WP-W3-01 lands
- ❌ OTLP/gRPC transport (HTTP/JSON only — gRPC requires
  `tonic` + protoc which is heavier than the bundle budget
  allows in Week 3)
- ❌ Resource attributes beyond `service.name` (host, version,
  etc. land when W3-09 surfaces a release-version constant)
- ❌ Metrics / logs export (this WP is traces-only)

## Acceptance criteria

- [ ] Migration `0005_span_export.sql` adds `exported_at` +
      `sampled_in` columns plus partial index
- [ ] Sampling at insert respects `NEURON_OTEL_SAMPLING_RATIO`
      (0.0 / 0.5 / 1.0 / unset = 1.0 default)
- [ ] `crate::telemetry` module exists with `mod.rs`, `otlp.rs`,
      `exporter.rs`
- [ ] `lib.rs::run().setup` starts the export loop iff
      `NEURON_OTEL_ENDPOINT` is set and non-empty
- [ ] Export loop is silent in the absence of the env var
      (no panic, no warn — clean no-op)
- [ ] OTLP JSON shape matches the v1.3 spec (fixture-based test)
- [ ] 2xx response flags `exported_at`; 4xx flags `-1`; 5xx /
      transport error leaves rows untouched
- [ ] All Week-2 tests still pass (regression: 110 + new tests)
- [ ] `pnpm gen:bindings:check` passes (no binding drift —
      this WP adds no Tauri commands)
- [ ] No `eprintln!`; `tracing::*` only
- [ ] `reqwest` configured with `rustls-tls` (no OpenSSL link)

## Verification commands

```bash
# Rust gate
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

# Bindings drift guard (must pass — no surface change)
pnpm gen:bindings:check

# Frontend gate (no expected change but verify)
pnpm typecheck
pnpm test --run
pnpm lint

# Sampling smoke (deterministic)
NEURON_OTEL_SAMPLING_RATIO=0.5 \
  cargo test --manifest-path src-tauri/Cargo.toml \
  -- telemetry::sampling_ratio_distribution

# Manual collector smoke (see §8 above)
```

## Notes / risks

- **`reqwest` dep weight**: ~2 MB compiled. Acceptable; the
  alternative (`hyper` + `rustls` directly) is more code for
  marginal binary-size gain. Pin to `0.12.x` exact.
- **`mockito` vs `wiremock` for tests**: pick whichever the
  sub-agent finds simpler — both support 2xx/4xx/5xx scripted
  responses. `wiremock` has slightly better async support;
  `mockito` is older and better-known.
- **Trace ID determinism**: hashing `run_id` to 16 bytes means
  re-exports of the same run produce the same `traceId`. This
  matters for the retry path — collectors deduplicate by
  `(traceId, spanId)`. The hash MUST be stable across versions;
  use `sha256(run_id)[..16]` and lock the choice in a `const`.
- **Float comparison in tests**: sampling tests use a
  binomial-distribution tolerance window
  (`±3σ` of expected hits). Do NOT use exact equality on
  random outcomes.
- **No `cancel_run` integration**: WP-W3-04 will add a cancel
  protocol; until then, in-flight spans (`duration_ms IS NULL`)
  are NOT exported. The query filter `duration_ms IS NOT NULL`
  enforces this.
- **Schema test count**: if WP-W3-01 lands first, the
  `migrations_are_idempotent` test already expects 4 — this WP
  bumps to 5. If W3-01 hasn't landed yet at sub-agent time,
  bump from 3 → 4. Either way, ONLY the count line changes.
- **No `runs_spans` row deletion in this WP**: the partial
  index assumes rows live forever. A future "trim" sweep that
  deletes spans older than N days will need to revisit the
  index; that's a follow-up concern, not this WP's.

## Sub-agent reminders

- Read `PROJECT_CHARTER.md` if uncertain about scope. Especially
  the tech-stack table — `reqwest` and `rand` are new deps but
  fall under the existing "any Rust crate" umbrella.
- This WP runs in **parallel** with WP-W3-01. Do NOT touch
  files claimed by W3-01 (`commands/me.rs`,
  `mcp/registry.rs::resolve_env`, anything secrets-related,
  any new `commands/secrets.rs` or `commands/settings.rs`).
- Do NOT add `opentelemetry`, `opentelemetry-otlp`, or
  `opentelemetry-sdk` crates. Hand-craft the JSON wire shape
  per §3 above. SDK adoption is a future refactor.
- Do NOT change the wire shape of `Span` in `models.rs` or
  any frontend-visible type. The new columns
  (`exported_at`, `sampled_in`) are backend-only — they MUST
  NOT appear in `Span` (`#[sqlx(default)]` is enough).
- Do NOT modify `commands/runs.rs::runs_get` to include the
  new columns. Frontend never reads them.
- Per AGENTS.md: one WP = one commit.
