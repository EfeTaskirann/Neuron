//! Coordinator FSM (WP-W3-12a §3).
//!
//! Pure Rust state machine that drives a swarm job through three
//! fixed stages:
//!
//! ```text
//! INIT  → SCOUT  → PLAN  → BUILD  → DONE
//!         (err)   (err)   (err)
//!         FAILED  FAILED  FAILED
//! ```
//!
//! `REVIEW` and `TEST` are reserved for W3-12d (reviewer +
//! integration-tester profiles); the FSM never enters them in 12a
//! and the next-state function asserts `debug_assert!(false, ...)`
//! if asked.
//!
//! No Coordinator LLM brain in 12a — Option A in the architectural
//! report §11.4. Swapping to Option B (single-shot `coordinator.md`
//! routing call) is a 1-2 file refactor in W3-12d; the FSM here is
//! deliberately a state-transition table so that swap can land
//! without rewriting the lifecycle plumbing.
//!
//! Cancellation / cleanup: `WorkspaceGuard` holds the workspace
//! lock for the full FSM run and releases it on `Drop`. This
//! covers the panic-unwind path so a panicked stage never leaks a
//! stuck workspace lock — the seatbelt also fires on the normal
//! return path (where `release_workspace` is idempotent so the
//! double-release is a no-op).

use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::Notify;

use crate::error::AppError;
use crate::events;
use crate::swarm::profile::{Profile, ProfileRegistry};
use crate::swarm::transport::Transport;

use super::decision::{
    parse_decision, CoordinatorDecision, CoordinatorRoute, CoordinatorScope,
};
use super::job::{
    Job, JobOutcome, JobRegistry, JobState, StageResult, SwarmJobEvent,
};
use super::verdict::{parse_verdict, Verdict, VerdictIssue, VerdictSeverity};

/// Maximum number of retries the FSM allows before falling through
/// to `Failed`. Exported as a `pub const` so W3-12d's Verdict-gated
/// retry loop doesn't have to relitigate the value when it lands.
/// The constant is **wired but not consumed** in 12a — there is no
/// retry logic in this WP.
pub const MAX_RETRIES: u32 = 2;

/// Specialist `Profile.id` strings the FSM dispatches in order. Pulled
/// out as `const` so the prompt-template tests can reuse them
/// without hardcoding strings in two places.
pub const SCOUT_ID: &str = "scout";
pub const PLANNER_ID: &str = "planner";
/// BackendBuilder profile id (W3-12a, renamed in W3-12h for symmetry
/// with `FRONTEND_BUILDER_ID`). Used when
/// `CoordinatorDecision.scope` is `Backend` or `Fullstack`
/// (Fullstack falls back to the backend chain in W3-12h; W3-12i
/// activates Fullstack sequential dispatch).
pub const BACKEND_BUILDER_ID: &str = "backend-builder";
/// BackendReviewer profile id (W3-12d, renamed in W3-12g). Runs
/// after `BACKEND_BUILDER_ID` and emits a JSON Verdict the FSM
/// gates on. Used when `CoordinatorDecision.scope` is `Backend`
/// or `Fullstack`. The const value (`"backend-reviewer"`) matches
/// the bundled profile (`backend-reviewer.md`).
pub const BACKEND_REVIEWER_ID: &str = "backend-reviewer";
/// FrontendBuilder profile id (W3-12g bundle, W3-12h dispatch).
/// Used when `CoordinatorDecision.scope` is `Frontend`. The const
/// value (`"frontend-builder"`) matches the bundled profile
/// (`frontend-builder.md`).
pub const FRONTEND_BUILDER_ID: &str = "frontend-builder";
/// FrontendReviewer profile id (W3-12g bundle, W3-12h dispatch).
/// Runs after `FRONTEND_BUILDER_ID` and emits a JSON Verdict the
/// FSM gates on. Used when `CoordinatorDecision.scope` is
/// `Frontend`. The const value (`"frontend-reviewer"`) matches the
/// bundled profile (`frontend-reviewer.md`).
pub const FRONTEND_REVIEWER_ID: &str = "frontend-reviewer";
/// IntegrationTester profile id (W3-12d). Runs after a successful
/// Reviewer verdict; emits a JSON Verdict the FSM gates on for
/// the final transition to `Done`.
pub const TESTER_ID: &str = "integration-tester";
/// Coordinator brain profile id (W3-12f). Runs once per job between
/// Scout and Plan; emits a JSON `CoordinatorDecision` that picks
/// between `ResearchOnly` (short-circuit to Done) and `ExecutePlan`
/// (continue the canonical chain).
pub const COORDINATOR_ID: &str = "coordinator";

/// Resolve specialist (builder, reviewer) pairs for a given scope
/// (W3-12i).
///
/// - `Backend` → one pair: backend-builder + backend-reviewer.
/// - `Frontend` → one pair: frontend-builder + frontend-reviewer.
/// - `Fullstack` → two pairs in dispatch order: backend pair first,
///   then frontend pair. The FSM run loop iterates this Vec
///   sequentially. W3-12j (future) parallelizes Fullstack via
///   `tokio::join!`; 12i keeps the FSM mental model linear.
///
/// Single-domain scopes return a 1-element Vec so the for-loop in
/// the run loop runs once — identical behavior to W3-12h's
/// `select_chain_ids`. The W3-12h helper is gone; no callers
/// remained post-12i.
fn select_chain_pairs(
    scope: CoordinatorScope,
) -> Vec<(&'static str, &'static str)> {
    match scope {
        CoordinatorScope::Backend => {
            vec![(BACKEND_BUILDER_ID, BACKEND_REVIEWER_ID)]
        }
        CoordinatorScope::Frontend => {
            vec![(FRONTEND_BUILDER_ID, FRONTEND_REVIEWER_ID)]
        }
        CoordinatorScope::Fullstack => vec![
            (BACKEND_BUILDER_ID, BACKEND_REVIEWER_ID),
            (FRONTEND_BUILDER_ID, FRONTEND_REVIEWER_ID),
        ],
    }
}

/// Domain hint piped into the BUILD prompt so the Builder knows
/// which side of a (potentially fullstack) plan to apply (W3-12i).
///
/// On Fullstack jobs the Plan covers both backend and frontend
/// steps; without a domain hint, BackendBuilder reading "Plan'ın
/// 1. adımı" would see step 1 = backend, but FrontendBuilder
/// reading the same plan would also pick step 1 — re-implementing
/// the backend step. The hint adds a single sentence steering the
/// Builder toward its own domain's steps.
///
/// Single-domain scopes still pass `Some(...)` (the hint is a
/// safe no-op there since the plan is already single-domain).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum BuilderDomain {
    Backend,
    Frontend,
}

/// Map a Builder profile id to its `BuilderDomain`. Defaults to
/// `Backend` for any unknown id — the run loop only ever feeds
/// this with the four canonical builder ids
/// (`BACKEND_BUILDER_ID` / `FRONTEND_BUILDER_ID`), and the
/// fallback keeps the function total without panicking on a
/// programmer bug.
fn builder_domain_for(builder_id: &str) -> BuilderDomain {
    if builder_id == FRONTEND_BUILDER_ID {
        BuilderDomain::Frontend
    } else {
        BuilderDomain::Backend
    }
}

/// SCOUT stage prompt template — wraps the goal as an
/// investigation request. WP §3 originally specified "goal
/// verbatim", but real `claude` runs (2026-05-05) showed Scout
/// burning its 6-turn budget oscillating when the goal was a
/// "do X" task, since Scout's persona forbids writes. The
/// wrapper restates the goal as "investigate this" so Scout
/// behaves consistently with its persona contract.
/// Substitutions: `{goal}`.
const SCOUT_PROMPT_TEMPLATE: &str =
    "Aşağıdaki görev için kod tabanını araştır ve ilgili dosyaları, \
     struct'ları, fonksiyonları ya da bağımlılıkları rapor et. \
     SEN KOD YAZMIYORSUN — sadece okuyup özetliyorsun.\n\
     \n\
     Görev:\n\
     \n\
     {goal}\n";

/// PLAN stage prompt template — Turkish. W3-12i adds `{scope}` so
/// the Planner knows whether to produce a single-domain plan or a
/// dual-domain (backend + frontend) plan for Fullstack goals.
/// Substitutions: `{goal}`, `{scope}`, `{scout_output}`.
const PLAN_PROMPT_TEMPLATE: &str = "Hedef: {goal}\n\
\n\
Kapsam: {scope}\n\
\n\
Scout bulguları:\n\
\n\
{scout_output}\n\
\n\
Bu hedef için adım adım bir plan üret. Kapsam fullstack ise \
plan hem backend (Rust/SQL) hem frontend (TS/React/CSS) \
adımlarını ayrı ayrı listelemeli; sıralama önemli — backend \
önce, frontend sonra.\n";

/// BUILD stage prompt template — Turkish. W3-12i adds a
/// `{domain_note}` placeholder steering the Builder toward its own
/// domain's steps when the upstream Plan is dual-domain (Fullstack).
/// On single-domain scopes the note still fires (Backend → "backend
/// tarafına bakıyorsun", Frontend → "frontend tarafına bakıyorsun")
/// — a no-op nudge that costs ~1 line but keeps the prompt shape
/// uniform across scopes. Substitutions: `{plan_output}`,
/// `{domain_note}`.
const BUILD_PROMPT_TEMPLATE: &str = "Aşağıdaki Plan'ın 1. adımını uygula.\n\
\n\
{plan_output}\n\
\n\
{domain_note}\n\
\n\
ŞU ANDA SADECE ADIM 1'İ UYGULA.\n";

/// REVIEW stage prompt template (W3-12d §7). Reviewer reads
/// Builder's changes against the original goal + plan and emits a
/// JSON Verdict. Substitutions: `{goal}`, `{plan}`, `{build}`.
///
/// The OUTPUT CONTRACT in the persona body is the authoritative
/// shape spec; this prompt restates it briefly so the LLM hits the
/// JSON-only response path even when the persona body is long.
const REVIEW_PROMPT_TEMPLATE: &str = "Görev: {goal}\n\
\n\
Plan:\n\
{plan}\n\
\n\
Builder'ın çıktısı:\n\
{build}\n\
\n\
Bu kodu ve değişiklikleri review et. Verdict'i tam olarak şu JSON \
şemasında ver, başka hiçbir şey yazma:\n\
\n\
{{ \"approved\": <bool>, \"issues\": [...], \"summary\": \"...\" }}\n";

/// TEST stage prompt template (W3-12d §7). IntegrationTester picks
/// the right test command for the project type and emits a JSON
/// Verdict over the result. Substitutions: `{goal}`, `{build}`.
const TEST_PROMPT_TEMPLATE: &str = "Görev: {goal}\n\
\n\
Builder'ın çıktısı:\n\
{build}\n\
\n\
İlgili test suite'ini çalıştır (cargo test / pnpm test / pytest, \
proje türüne göre). Verdict'i şu JSON şemasında ver, başka hiçbir \
şey yazma:\n\
\n\
{{ \"approved\": <bool>, \"issues\": [...], \"summary\": \"...\" }}\n";

/// CLASSIFY stage prompt template (W3-12f §5). The Coordinator
/// brain reads goal + Scout findings and returns a JSON
/// `CoordinatorDecision`. Substitutions: `{goal}`, `{scout_output}`.
///
/// The OUTPUT CONTRACT in the persona body is the authoritative
/// shape spec; this prompt restates it briefly so the LLM hits the
/// JSON-only response path even when the persona body is long.
/// `{{` / `}}` are literal braces (escape for `format!`-style; we
/// use `.replace` here so the curly braces stand verbatim in the
/// rendered output).
const CLASSIFY_PROMPT_TEMPLATE: &str = "Hedef:\n\
\n\
{goal}\n\
\n\
Scout bulguları:\n\
\n\
{scout_output}\n\
\n\
Bu hedef için uygun rotayı seç. Çıktı sadece bir JSON objesi olmalı:\n\
{{ \"route\": \"research_only\" | \"execute_plan\", \"reasoning\": \"...\" }}\n";

/// PLAN stage prompt template used on the retry path (W3-12e §4).
/// Carries the previous plan + the rejecting Verdict's issues so the
/// Planner can produce a corrected plan rather than starting from
/// scratch. The `{gate}` substitution is `"Reviewer"` or
/// `"IntegrationTester"` — the human-readable label of the gate that
/// triggered the retry. W3-12i adds `{scope}` so the retry plan stays
/// scope-aware (Fullstack retries must still cover both domains).
/// Substitutions: `{goal}`, `{scope}`, `{scout_output}`,
/// `{verdict_issues}`, `{previous_plan}`, `{gate}`.
const RETRY_PLAN_PROMPT_TEMPLATE: &str = "Hedef: {goal}\n\
\n\
Kapsam: {scope}\n\
\n\
Scout bulguları:\n\
\n\
{scout_output}\n\
\n\
ÖNCEKİ DENEMENİZ {gate} aşamasında REDDEDİLDİ. {gate}'ın bulduğu \
sorunlar:\n\
\n\
{verdict_issues}\n\
\n\
Önceki plan (reddedildi):\n\
\n\
{previous_plan}\n\
\n\
Bu sorunları çözecek yeni bir plan üret. Yalnızca reddedilen \
sorunlara odaklan; mevcut başarılı kısımları yeniden tasarlama. \
Kapsam fullstack ise plan hem backend hem frontend adımlarını \
kapsamalı.\n";

/// Render the SCOUT prompt by substituting `{goal}`. Free fn so
/// the prompt-template test can call it without a full FSM.
fn render_scout_prompt(goal: &str) -> String {
    SCOUT_PROMPT_TEMPLATE.replace("{goal}", goal)
}

/// Render the PLAN prompt by substituting `{goal}`, `{scope}`, and
/// `{scout_output}`. Pulled out as a free fn so the prompt-template
/// test can call it without instantiating a full FSM. W3-12i adds
/// `scope` so the Planner can produce a dual-domain plan for
/// Fullstack goals (see `PLAN_PROMPT_TEMPLATE`).
fn render_plan_prompt(
    goal: &str,
    scout_output: &str,
    scope: CoordinatorScope,
) -> String {
    PLAN_PROMPT_TEMPLATE
        .replace("{goal}", goal)
        .replace("{scope}", scope_label(scope))
        .replace("{scout_output}", scout_output)
}

/// Render the BUILD prompt by substituting `{plan_output}` and
/// `{domain_note}`. W3-12i adds `builder_domain` so the FSM can
/// steer a Fullstack run's BackendBuilder vs FrontendBuilder toward
/// its own half of the dual-domain plan. `None` is a backwards-compat
/// path for tests that exercise the helper directly; the run loop
/// always passes `Some(...)`.
fn render_build_prompt(
    plan_output: &str,
    builder_domain: Option<BuilderDomain>,
) -> String {
    let domain_note = match builder_domain {
        Some(BuilderDomain::Backend) => {
            "Sen backend tarafına bakıyorsun (Rust / SQL / Tauri \
             komutları). Plan'daki backend adımlarına odaklan; \
             frontend adımlarını atla."
        }
        Some(BuilderDomain::Frontend) => {
            "Sen frontend tarafına bakıyorsun (React / TS / CSS). \
             Plan'daki frontend adımlarına odaklan; backend \
             adımlarını atla."
        }
        None => "",
    };
    BUILD_PROMPT_TEMPLATE
        .replace("{plan_output}", plan_output)
        .replace("{domain_note}", domain_note)
}

/// Wire-form label for a scope (W3-12i). Matches the snake_case
/// `#[serde(rename_all = "snake_case")]` enum encoding so the
/// Planner sees the same string the persisted `decision.scope`
/// carries.
fn scope_label(scope: CoordinatorScope) -> &'static str {
    match scope {
        CoordinatorScope::Backend => "backend",
        CoordinatorScope::Frontend => "frontend",
        CoordinatorScope::Fullstack => "fullstack",
    }
}

/// Render the CLASSIFY prompt by substituting `{goal}` and
/// `{scout_output}` (W3-12f). Free fn so the prompt-template test
/// can call it without instantiating a full FSM.
fn render_classify_prompt(goal: &str, scout_output: &str) -> String {
    CLASSIFY_PROMPT_TEMPLATE
        .replace("{goal}", goal)
        .replace("{scout_output}", scout_output)
}

/// Render the REVIEW prompt by substituting `{goal}`, `{plan}`, and
/// `{build}`. Free fn so tests can call it without a full FSM.
fn render_review_prompt(
    goal: &str,
    plan: &str,
    build: &str,
) -> String {
    REVIEW_PROMPT_TEMPLATE
        .replace("{goal}", goal)
        .replace("{plan}", plan)
        .replace("{build}", build)
}

/// Render the TEST prompt by substituting `{goal}` and `{build}`.
/// Free fn so tests can call it without a full FSM.
fn render_test_prompt(goal: &str, build: &str) -> String {
    TEST_PROMPT_TEMPLATE
        .replace("{goal}", goal)
        .replace("{build}", build)
}

/// Render the retry-Plan prompt (W3-12e §4). `gate` must be either
/// `JobState::Review` or `JobState::Test`; the human-readable label
/// `"Reviewer"` / `"IntegrationTester"` is interpolated into the
/// prompt. Other states would be a programmer error — the run loop
/// only invokes this helper from the Verdict-rejected branches of
/// the Review/Test gates.
///
/// `{verdict_issues}` is rendered as a Markdown-style bullet list
/// via `verdict_issues_as_bullets`; an empty issue list falls back
/// to a sentinel string so the prompt body always contains a
/// non-empty issues section.
fn render_retry_plan_prompt(
    goal: &str,
    scout_output: &str,
    previous_plan: &str,
    verdict: &Verdict,
    gate: JobState,
    scope: CoordinatorScope,
) -> String {
    let gate_label = retry_gate_label(gate);
    let issues = verdict_issues_as_bullets(&verdict.issues);
    RETRY_PLAN_PROMPT_TEMPLATE
        .replace("{goal}", goal)
        .replace("{scope}", scope_label(scope))
        .replace("{scout_output}", scout_output)
        .replace("{verdict_issues}", &issues)
        .replace("{previous_plan}", previous_plan)
        // `{gate}` appears twice in the template so a single
        // `replace` covers both occurrences.
        .replace("{gate}", gate_label)
}

/// Human-readable label for the retry-triggering gate. Surfaces in
/// the retry-Plan prompt so the Planner reads "Reviewer aşamasında"
/// or "IntegrationTester aşamasında" instead of the wire form.
///
/// Falls back to `"Reviewer"` for any non-Review/Test state — the
/// retry loop never calls this with another state, but the fallback
/// keeps the function total and avoids a `panic!` in a hot path.
fn retry_gate_label(state: JobState) -> &'static str {
    match state {
        JobState::Test => "IntegrationTester",
        // Review is the canonical default; any other state would be
        // a programmer bug elsewhere in the FSM and the surface that
        // calls this helper already filters to {Review, Test}.
        _ => "Reviewer",
    }
}

/// Render a Verdict's issue list as a Markdown bullet list. Each
/// line is `- [{severity}] {file}:{line}: {message}`, with omitted
/// `file`/`line` collapsed to a single em-dash so the bullet stays
/// aligned. An empty issue list returns a sentinel string so the
/// retry prompt body always contains a non-empty issues section.
///
/// Pulled out as a private fn (not a method on `Verdict`) so the
/// surface that knows about prompt templates owns the rendering;
/// `Verdict` itself stays a pure data type.
fn verdict_issues_as_bullets(issues: &[VerdictIssue]) -> String {
    if issues.is_empty() {
        return "(Ayrıntılı issue listesi sağlanmadı.)".to_string();
    }
    issues
        .iter()
        .map(|i| {
            let loc = match (&i.file, &i.line) {
                (Some(f), Some(l)) => format!("{f}:{l}"),
                (Some(f), None) => f.clone(),
                _ => "—".to_string(),
            };
            let sev = match i.severity {
                VerdictSeverity::High => "high",
                VerdictSeverity::Med => "med",
                VerdictSeverity::Low => "low",
            };
            format!("- [{sev}] {loc}: {msg}", msg = i.message)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pure-Rust transition table for the happy path. The FSM run loop
/// walks a fixed sequence (Scout → Plan → Build → Review → Test →
/// Done) and additionally branches on the parsed `Verdict.approved`
/// at the Review and Test gates — the `bool` here represents the
/// stage's *invocation* outcome (Ok / Err from `transport.invoke`),
/// NOT the verdict. Verdict-driven branching lives at the call
/// sites in `run_job_inner` because it requires unwrapping the
/// parsed Verdict value, which is a structural mismatch with this
/// boolean transition table.
#[allow(dead_code)] // Test helper; run loop is fixed.
pub(crate) fn next_state(current: JobState, ok: bool) -> JobState {
    match (current, ok) {
        (JobState::Init, _) => JobState::Scout,
        (JobState::Scout, true) => JobState::Classify,
        (JobState::Scout, false) => JobState::Failed,
        // W3-12f: Classify is the Coordinator brain decision point.
        // `(Classify, true)` assumes ExecutePlan; the run loop owns
        // the ResearchOnly → Done short-circuit because that branch
        // depends on the parsed `CoordinatorDecision.route` rather
        // than the invoke's bool outcome.
        (JobState::Classify, true) => JobState::Plan,
        (JobState::Classify, false) => JobState::Failed,
        (JobState::Plan, true) => JobState::Build,
        (JobState::Plan, false) => JobState::Failed,
        (JobState::Build, true) => JobState::Review,
        (JobState::Build, false) => JobState::Failed,
        // (Review, true) is the success edge of the invoke; the FSM
        // additionally requires `Verdict.approved=true` to actually
        // advance past Review. See `run_job_inner` for the verdict
        // branching.
        (JobState::Review, true) => JobState::Test,
        (JobState::Review, false) => JobState::Failed,
        (JobState::Test, true) => JobState::Done,
        (JobState::Test, false) => JobState::Failed,
        (JobState::Done | JobState::Failed, _) => current,
    }
}

/// The Coordinator state machine. Generic over `T: Transport` so
/// tests can substitute `MockTransport` without rebuilding the
/// production code path. The substrate-side `SubprocessTransport`
/// is the production wiring.
pub struct CoordinatorFsm<T: Transport> {
    profiles: Arc<ProfileRegistry>,
    transport: T,
    registry: Arc<JobRegistry>,
    /// Per-stage timeout budget — handed verbatim to
    /// `transport.invoke`. Default 60s (matches the W3-11
    /// substrate); IPC layer reads `NEURON_SWARM_STAGE_TIMEOUT_SEC`
    /// to override.
    stage_timeout: Duration,
}

impl<T: Transport> CoordinatorFsm<T> {
    /// Build an FSM bound to the given profiles, transport, and
    /// registry. Each `swarm:run_job` IPC creates a fresh FSM —
    /// there is no shared FSM in 12a; the `JobRegistry` is the only
    /// shared state.
    pub fn new(
        profiles: Arc<ProfileRegistry>,
        transport: T,
        registry: Arc<JobRegistry>,
        stage_timeout: Duration,
    ) -> Self {
        Self {
            profiles,
            transport,
            registry,
            stage_timeout,
        }
    }

    /// Test-only: drive a job with a caller-supplied `job_id`.
    /// Lets unit tests subscribe to `swarm:job:{id}:event` *before*
    /// the FSM emits any event, eliminating the race between
    /// listener-attach and `Started`. Production callers must use
    /// `run_job` (which mints a fresh ULID-based id) — that's the
    /// only way the WorkspaceBusy / cross-window concurrency
    /// guarantees hold.
    #[cfg(test)]
    pub(crate) async fn run_job_with_id<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        job_id: String,
        workspace_id: String,
        goal: String,
    ) -> Result<JobOutcome, AppError> {
        self.run_job_inner(app, Some(job_id), workspace_id, goal).await
    }

    /// Drive a job from `Init` to `Done` / `Failed`. Blocking;
    /// returns the final outcome. Mutates the registry at every
    /// transition and emits one `SwarmJobEvent` per transition on
    /// the `swarm:job:{job_id}:event` channel (WP-W3-12c).
    ///
    /// On the failure path the FSM does NOT push a `StageResult`
    /// for the failing stage — only successful stages land in
    /// `stages`. The failure is encoded in `Job.last_error` /
    /// `JobOutcome.last_error`.
    ///
    /// On cancellation (via `JobRegistry::signal_cancel`) the FSM
    /// emits `Cancelled { cancelled_during: <state> }` then
    /// finalizes the job as `Failed` with
    /// `last_error = Some("cancelled by user")` and emits the
    /// trailing `Finished` event.
    pub async fn run_job<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        workspace_id: String,
        goal: String,
    ) -> Result<JobOutcome, AppError> {
        self.run_job_inner(app, None, workspace_id, goal).await
    }

    /// Inner implementation backing both `run_job` (mints id) and
    /// `run_job_with_id` (test-only; preset id). Keeps the lifecycle
    /// logic in one place.
    async fn run_job_inner<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        preset_job_id: Option<String>,
        workspace_id: String,
        goal: String,
    ) -> Result<JobOutcome, AppError> {
        // 1. Pre-flight validation. Both checks fire before the
        //    lock map is touched so a malformed call never reserves
        //    a workspace.
        if workspace_id.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "workspaceId must not be empty".into(),
            ));
        }
        if goal.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "goal must not be empty".into(),
            ));
        }

        // 2. Mint the job + acquire the per-workspace lock atomically.
        let job_id = preset_job_id
            .unwrap_or_else(|| format!("j-{}", ulid::Ulid::new()));
        let now_ms = current_unix_millis();
        let job = Job {
            id: job_id.clone(),
            goal: goal.clone(),
            created_at_ms: now_ms,
            state: JobState::Init,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
            last_verdict: None,
        };
        self.registry
            .try_acquire_workspace(&workspace_id, job)
            .await?;

        // 3. Hold the workspace lock through the entire run via
        //    a Drop guard — protects the panic-unwind path.
        let _ws_guard = WorkspaceGuard {
            registry: Arc::clone(&self.registry),
            workspace_id: workspace_id.clone(),
            job_id: job_id.clone(),
        };

        // 4. Register a cancellation Notify and hold a Drop guard so
        //    the entry is unregistered on every exit path (success,
        //    failure, panic, cancellation). The notify itself is
        //    consumed by the per-stage `tokio::select!` below.
        let notify = Arc::new(Notify::new());
        self.registry
            .register_cancel(&job_id, Arc::clone(&notify))?;
        let _cancel_guard = CancelGuard {
            registry: Arc::clone(&self.registry),
            job_id: job_id.clone(),
        };

        // 5. Resolve the three specialist profiles up front so a
        //    missing-profile error surfaces before we spawn anything.
        //    `cloned()` is cheap (Profile holds Strings, not handles).
        let scout = self
            .profiles
            .get(SCOUT_ID)
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "swarm profile `{SCOUT_ID}` (required for FSM)"
                ))
            })?
            .clone();
        let planner = self
            .profiles
            .get(PLANNER_ID)
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "swarm profile `{PLANNER_ID}` (required for FSM)"
                ))
            })?
            .clone();
        // W3-12h: Builder + Reviewer are resolved INSIDE the retry
        // loop based on `decision.scope` (set by the Classify stage
        // below). The decision is stable across retry iterations for
        // 12h (a single job's scope doesn't change mid-flight), but
        // the resolution happens per-iteration so future per-domain
        // retry logic (W3-12i+) can pivot the chain without
        // restructuring the run loop. `scout`, `planner`, `tester`,
        // and `coordinator` stay scope-agnostic and are still
        // resolved up front.
        let tester = self
            .profiles
            .get(TESTER_ID)
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "swarm profile `{TESTER_ID}` (required for FSM)"
                ))
            })?
            .clone();
        // W3-12f: Coordinator brain — resolved up front so a missing
        // profile fails the job before any stage spawns. The
        // brain runs exactly once per job (in the Classify stage
        // between Scout and Plan).
        let coordinator = self
            .profiles
            .get(COORDINATOR_ID)
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "swarm profile `{COORDINATOR_ID}` (required for FSM)"
                ))
            })?
            .clone();

        // 6. Emit `Started` once the workspace lock + cancel notify
        //    are both in place.
        emit_swarm_event(
            app,
            &job_id,
            &SwarmJobEvent::Started {
                job_id: job_id.clone(),
                workspace_id: workspace_id.clone(),
                goal: goal.clone(),
                created_at_ms: now_ms,
            },
        );

        // 7. Walk the chain. Each stage races its future against
        //    `notify.notified()`; cancellation short-circuits the
        //    chain by emitting `Cancelled` and finalizing as Failed.
        //
        //    W3-12e: SCOUT runs once, then PLAN/BUILD/REVIEW/TEST
        //    sit inside `'retry_loop` so a Verdict-rejected gate can
        //    loop back to PLAN with the rejection feedback piped
        //    into the prompt. The retry budget is `MAX_RETRIES`;
        //    rejections beyond it fall through to `Failed`.

        // SCOUT stage — runs once per job, even across retries.
        let scout_prompt = render_scout_prompt(&goal);
        let scout_text = match self
            .run_stage_with_cancel(
                app,
                JobState::Scout,
                &scout,
                &scout_prompt,
                &job_id,
                &notify,
            )
            .await
        {
            StageOutcome::Ok(stage) => {
                let text = stage.assistant_text.clone();
                emit_swarm_event(
                    app,
                    &job_id,
                    &SwarmJobEvent::StageCompleted {
                        job_id: job_id.clone(),
                        stage: stage.clone(),
                    },
                );
                self.registry
                    .update(&job_id, |j| {
                        j.stages.push(stage);
                    })
                    .await?;
                text
            }
            StageOutcome::Err(e) => {
                self.finalize_failed(
                    &job_id,
                    &workspace_id,
                    Some(e.to_string()),
                )
                .await?;
                return self.emit_finished_and_build(app, &job_id);
            }
            StageOutcome::Cancelled => {
                self.finalize_cancelled(&job_id, &workspace_id).await?;
                return self.emit_finished_and_build(app, &job_id);
            }
        };

        // CLASSIFY stage (W3-12f) — single-shot Coordinator brain.
        // Runs exactly once per job between Scout and Plan. The
        // parsed `CoordinatorDecision` decides whether the FSM
        // short-circuits to Done (ResearchOnly) or falls through to
        // the Plan/Build/Review/Test chain (ExecutePlan).
        //
        // Default-fail-open contract: an unparseable Coordinator
        // output is logged at `warn` and falls through to
        // ExecutePlan. Misclassifying a research-only goal as
        // execute is "wasted ~$0.10 on empty Plan/Build outputs";
        // misclassifying an execute goal as research-only is "the
        // user thinks the job succeeded but no code was written".
        // We default to the safer (more expensive) branch.
        let classify_prompt =
            render_classify_prompt(&goal, &scout_text);
        let decision = match self
            .run_stage_with_cancel(
                app,
                JobState::Classify,
                &coordinator,
                &classify_prompt,
                &job_id,
                &notify,
            )
            .await
        {
            StageOutcome::Ok(stage) => {
                let decision = match parse_decision(&stage.assistant_text) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(
                            %job_id,
                            error = %e,
                            "swarm: Coordinator decision unparseable; defaulting to ExecutePlan"
                        );
                        CoordinatorDecision {
                            route: CoordinatorRoute::ExecutePlan,
                            // W3-12g: fallback path picks the safest
                            // scope (Backend) — matches the FSM's
                            // existing single-chain dispatch and the
                            // legacy-shape default in the parser.
                            scope: CoordinatorScope::Backend,
                            reasoning: format!(
                                "fallback: brain output unparseable ({e})"
                            ),
                        }
                    }
                };
                // W3-12g §6 — record route + scope for audit. The
                // scope drives Builder + Reviewer selection in the
                // retry loop below via `select_chain_pairs`. The
                // `tracing::info!` line stays as the canonical audit
                // marker for every job (route + scope).
                //
                // W3-12i: scope=Fullstack now dispatches the full
                // sequential chain (BB+BR then FB+FR); the W3-12h
                // fallback warn is dropped.
                tracing::info!(
                    job_id = %job_id,
                    route = ?decision.route,
                    scope = ?decision.scope,
                    "swarm: Coordinator decision recorded"
                );
                // Stamp the parsed (or fallback) decision onto the
                // StageResult so it lands in `swarm_stages.decision_json`
                // via the registry's SQL write-through. We emit
                // `StageCompleted` with the decision-bearing stage so
                // the W3-14 UI sees the canonical shape.
                let stage_with_decision = StageResult {
                    coordinator_decision: Some(decision.clone()),
                    ..stage
                };
                emit_swarm_event(
                    app,
                    &job_id,
                    &SwarmJobEvent::StageCompleted {
                        job_id: job_id.clone(),
                        stage: stage_with_decision.clone(),
                    },
                );
                self.registry
                    .update(&job_id, |j| {
                        j.stages.push(stage_with_decision);
                    })
                    .await?;
                emit_swarm_event(
                    app,
                    &job_id,
                    &SwarmJobEvent::DecisionMade {
                        job_id: job_id.clone(),
                        decision: decision.clone(),
                    },
                );
                decision
            }
            StageOutcome::Err(e) => {
                self.finalize_failed(
                    &job_id,
                    &workspace_id,
                    Some(e.to_string()),
                )
                .await?;
                return self.emit_finished_and_build(app, &job_id);
            }
            StageOutcome::Cancelled => {
                self.finalize_cancelled(&job_id, &workspace_id).await?;
                return self.emit_finished_and_build(app, &job_id);
            }
        };

        // Branch on the decision. ResearchOnly short-circuits the
        // chain — Scout's findings are the deliverable. ExecutePlan
        // falls through to the canonical Plan/Build/Review/Test
        // retry loop below.
        match decision.route {
            CoordinatorRoute::ResearchOnly => {
                self.registry
                    .update(&job_id, |j| {
                        j.state = JobState::Done;
                    })
                    .await?;
                self.registry
                    .release_workspace(&workspace_id, &job_id)
                    .await;
                return self.emit_finished_and_build(app, &job_id);
            }
            CoordinatorRoute::ExecutePlan => {
                // Fall through to the canonical retry loop.
            }
        }

        // 7b. Plan/Build/Review/Test loop. The first iteration runs
        //     with the canonical Plan prompt; subsequent iterations
        //     fire only on a Verdict rejection within the retry
        //     budget and rebuild Plan from the rejecting verdict's
        //     issues + the previous plan via
        //     `render_retry_plan_prompt`. `last_plan_text` keeps the
        //     prior plan around so the retry prompt can quote it
        //     verbatim.
        //
        //     W3-12i: Build/Review now iterates over
        //     `select_chain_pairs(decision.scope)`. Backend/Frontend
        //     return one pair → for-loop runs once (identical
        //     behavior to W3-12h). Fullstack returns two pairs →
        //     BB+BR runs first, then FB+FR; rejection at any pair's
        //     Review gate triggers `try_start_retry` and re-runs the
        //     whole Plan + pairs sequence (per-domain retry is a
        //     future polish; see WP §"Notes / risks").
        let mut last_plan_text: Option<String> = None;
        'retry_loop: loop {
            // PLAN — branches on `retry_count`. The first attempt
            // uses the canonical template; retries quote the prior
            // plan + verdict issues. The Plan covers all domains
            // (Fullstack plan = backend section + frontend section);
            // the per-pair BUILD prompt narrows via `BuilderDomain`.
            let plan_prompt = {
                let snapshot = self.registry.get(&job_id).ok_or_else(|| {
                    AppError::Internal(format!(
                        "swarm job `{job_id}` vanished from registry mid-loop"
                    ))
                })?;
                if snapshot.retry_count == 0 {
                    render_plan_prompt(&goal, &scout_text, decision.scope)
                } else {
                    let prev_plan = last_plan_text.as_deref().unwrap_or("");
                    // `last_verdict` is set by the rejection branch
                    // before the loop continues; absence here would
                    // be a programmer bug, but we degrade rather
                    // than panic in a hot path.
                    let prev_verdict = snapshot
                        .last_verdict
                        .clone()
                        .unwrap_or_else(|| Verdict {
                            approved: false,
                            issues: Vec::new(),
                            summary: String::new(),
                        });
                    let gate = snapshot
                        .last_rejecting_gate()
                        .unwrap_or(JobState::Review);
                    render_retry_plan_prompt(
                        &goal,
                        &scout_text,
                        prev_plan,
                        &prev_verdict,
                        gate,
                        decision.scope,
                    )
                }
            };

            let plan_text = match self
                .run_stage_with_cancel(
                    app,
                    JobState::Plan,
                    &planner,
                    &plan_prompt,
                    &job_id,
                    &notify,
                )
                .await
            {
                StageOutcome::Ok(stage) => {
                    let text = stage.assistant_text.clone();
                    emit_swarm_event(
                        app,
                        &job_id,
                        &SwarmJobEvent::StageCompleted {
                            job_id: job_id.clone(),
                            stage: stage.clone(),
                        },
                    );
                    self.registry
                        .update(&job_id, |j| {
                            j.stages.push(stage);
                        })
                        .await?;
                    text
                }
                StageOutcome::Err(e) => {
                    self.finalize_failed(
                        &job_id,
                        &workspace_id,
                        Some(e.to_string()),
                    )
                    .await?;
                    return self.emit_finished_and_build(app, &job_id);
                }
                StageOutcome::Cancelled => {
                    self.finalize_cancelled(&job_id, &workspace_id).await?;
                    return self.emit_finished_and_build(app, &job_id);
                }
            };
            last_plan_text = Some(plan_text.clone());

            // BUILD + REVIEW pairs (W3-12i). For Backend/Frontend
            // this loop runs once; for Fullstack it runs twice
            // (backend pair, then frontend pair). `last_build_text`
            // tracks the most recent Builder output so the TEST
            // gate after the loop has something to feed downstream
            // (Fullstack = the FrontendBuilder's text; matches the
            // sequential narrative).
            let pairs = select_chain_pairs(decision.scope);
            let mut last_build_text: Option<String> = None;
            for (builder_id, reviewer_id) in &pairs {
                let builder = self
                    .profiles
                    .get(builder_id)
                    .ok_or_else(|| {
                        AppError::Internal(format!(
                            "missing profile id `{builder_id}`"
                        ))
                    })?
                    .clone();
                let reviewer = self
                    .profiles
                    .get(reviewer_id)
                    .ok_or_else(|| {
                        AppError::Internal(format!(
                            "missing profile id `{reviewer_id}`"
                        ))
                    })?
                    .clone();
                let domain = builder_domain_for(builder_id);

                // BUILD.
                let build_prompt =
                    render_build_prompt(&plan_text, Some(domain));
                let build_text = match self
                    .run_stage_with_cancel(
                        app,
                        JobState::Build,
                        &builder,
                        &build_prompt,
                        &job_id,
                        &notify,
                    )
                    .await
                {
                    StageOutcome::Ok(stage) => {
                        let text = stage.assistant_text.clone();
                        emit_swarm_event(
                            app,
                            &job_id,
                            &SwarmJobEvent::StageCompleted {
                                job_id: job_id.clone(),
                                stage: stage.clone(),
                            },
                        );
                        self.registry
                            .update(&job_id, |j| {
                                j.stages.push(stage);
                            })
                            .await?;
                        text
                    }
                    StageOutcome::Err(e) => {
                        self.finalize_failed(
                            &job_id,
                            &workspace_id,
                            Some(e.to_string()),
                        )
                        .await?;
                        return self.emit_finished_and_build(app, &job_id);
                    }
                    StageOutcome::Cancelled => {
                        self.finalize_cancelled(&job_id, &workspace_id).await?;
                        return self.emit_finished_and_build(app, &job_id);
                    }
                };
                last_build_text = Some(build_text.clone());

                // REVIEW gate for this pair.
                let review_prompt =
                    render_review_prompt(&goal, &plan_text, &build_text);
                match self
                    .run_verdict_stage(
                        app,
                        JobState::Review,
                        &reviewer,
                        &review_prompt,
                        &job_id,
                        &notify,
                    )
                    .await?
                {
                    VerdictStageOutcome::Approved(_) => {
                        // Continue to the next pair (or fall through
                        // to the TEST gate when this was the last
                        // pair).
                    }
                    VerdictStageOutcome::Rejected(verdict) => {
                        if self
                            .try_start_retry(
                                app,
                                &job_id,
                                JobState::Review,
                                verdict.clone(),
                            )
                            .await?
                        {
                            // Re-run the WHOLE pairs sequence from
                            // PLAN. Wasteful for Fullstack on a
                            // late-pair rejection (re-runs already-
                            // approved earlier pair) but correct;
                            // per-domain retry is a future polish.
                            continue 'retry_loop;
                        }
                        self.finalize_failed_with_verdict(
                            &job_id,
                            &workspace_id,
                            verdict,
                        )
                        .await?;
                        return self.emit_finished_and_build(app, &job_id);
                    }
                    VerdictStageOutcome::ParseFailed(e) => {
                        self.finalize_failed(
                            &job_id,
                            &workspace_id,
                            Some(e.to_string()),
                        )
                        .await?;
                        return self.emit_finished_and_build(app, &job_id);
                    }
                    VerdictStageOutcome::InvokeError(e) => {
                        self.finalize_failed(
                            &job_id,
                            &workspace_id,
                            Some(e.to_string()),
                        )
                        .await?;
                        return self.emit_finished_and_build(app, &job_id);
                    }
                    VerdictStageOutcome::Cancelled => {
                        self.finalize_cancelled(&job_id, &workspace_id).await?;
                        return self.emit_finished_and_build(app, &job_id);
                    }
                }
            }

            // All pairs approved. Run the TEST gate against the
            // most recent Builder output. `expect`-free: the for-
            // loop above always populates `last_build_text` because
            // `select_chain_pairs` never returns an empty Vec —
            // every CoordinatorScope variant maps to ≥1 pair.
            let build_text = last_build_text.unwrap_or_default();

            // TEST gate.
            let test_prompt = render_test_prompt(&goal, &build_text);
            match self
                .run_verdict_stage(
                    app,
                    JobState::Test,
                    &tester,
                    &test_prompt,
                    &job_id,
                    &notify,
                )
                .await?
            {
                VerdictStageOutcome::Approved(_) => {}
                VerdictStageOutcome::Rejected(verdict) => {
                    if self
                        .try_start_retry(
                            app,
                            &job_id,
                            JobState::Test,
                            verdict.clone(),
                        )
                        .await?
                    {
                        continue 'retry_loop;
                    }
                    self.finalize_failed_with_verdict(
                        &job_id,
                        &workspace_id,
                        verdict,
                    )
                    .await?;
                    return self.emit_finished_and_build(app, &job_id);
                }
                VerdictStageOutcome::ParseFailed(e) => {
                    self.finalize_failed(
                        &job_id,
                        &workspace_id,
                        Some(e.to_string()),
                    )
                    .await?;
                    return self.emit_finished_and_build(app, &job_id);
                }
                VerdictStageOutcome::InvokeError(e) => {
                    self.finalize_failed(
                        &job_id,
                        &workspace_id,
                        Some(e.to_string()),
                    )
                    .await?;
                    return self.emit_finished_and_build(app, &job_id);
                }
                VerdictStageOutcome::Cancelled => {
                    self.finalize_cancelled(&job_id, &workspace_id).await?;
                    return self.emit_finished_and_build(app, &job_id);
                }
            }

            // All gates approved — happy path.
            // 8. Mark Done and release the workspace lock. The Drop
            //    guards will also fire on scope exit — both
            //    `release_workspace` and `unregister_cancel` are
            //    idempotent.
            self.registry
                .update(&job_id, |j| {
                    j.state = JobState::Done;
                })
                .await?;
            self.registry
                .release_workspace(&workspace_id, &job_id)
                .await;
            return self.emit_finished_and_build(app, &job_id);
        }
    }

    /// Increment `retry_count`, stamp the rejecting Verdict on the
    /// job, fire `RetryStarted`, and signal the run loop to continue.
    /// Returns `Ok(true)` if a retry was started, `Ok(false)` if the
    /// retry budget is exhausted (caller should finalize the job
    /// as Failed-with-verdict).
    ///
    /// `attempt` on the wire is 1-indexed: the first retry is
    /// `attempt=2`, the second is `attempt=3`. The pre-increment
    /// `retry_count` is the count of retries that *finished*; this
    /// helper bumps it to "the count of retries that have started"
    /// before firing the event so the wire value reads as the
    /// upcoming attempt number.
    async fn try_start_retry<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        job_id: &str,
        gate: JobState,
        verdict: Verdict,
    ) -> Result<bool, AppError> {
        let snapshot = self.registry.get(job_id).ok_or_else(|| {
            AppError::Internal(format!(
                "swarm job `{job_id}` vanished from registry mid-retry"
            ))
        })?;
        if snapshot.retry_count >= MAX_RETRIES {
            return Ok(false);
        }
        // Increment retry_count, transition state back to Plan, and
        // stamp `last_verdict` so the retry-Plan prompt can quote
        // the rejection. We do this in one `update` so the SQL
        // write-through sees the consistent shape.
        self.registry
            .update(job_id, |j| {
                j.retry_count += 1;
                j.state = JobState::Plan;
                j.last_verdict = Some(verdict.clone());
            })
            .await?;
        let attempt = snapshot.retry_count.saturating_add(2);
        emit_swarm_event(
            app,
            job_id,
            &SwarmJobEvent::RetryStarted {
                job_id: job_id.to_string(),
                attempt,
                max_retries: MAX_RETRIES,
                triggered_by: gate,
                verdict,
            },
        );
        Ok(true)
    }

    /// Run a Review or Test stage end-to-end: invoke the specialist,
    /// parse the Verdict from `assistant_text`, append the stage,
    /// emit `StageCompleted`. Pulled into a helper because both
    /// gates have the same shape (only the prompt + state differ).
    ///
    /// W3-12e refactor: this helper no longer finalizes the job on
    /// rejection / parse-failure / invoke-error — the run loop owns
    /// that decision because it needs the rejecting Verdict to feed
    /// the retry-Plan prompt (or, on retry-budget exhaustion, to
    /// stamp `last_verdict` on the Failed job).
    ///
    /// Returns:
    ///
    /// - `Approved(verdict)` — invoke + parse succeeded;
    ///   `approved=true`. Stage appended with `verdict` populated.
    /// - `Rejected(verdict)` — invoke + parse succeeded;
    ///   `approved=false`. Stage appended with the verdict; the run
    ///   loop decides retry vs. finalize.
    /// - `ParseFailed(err)` — invoke succeeded but the assistant
    ///   text wasn't a parseable Verdict. Stage IS appended with
    ///   `verdict=None` so the W3-14 UI sees the raw text.
    /// - `InvokeError(err)` — `transport.invoke` returned an error.
    ///   No stage appended.
    /// - `Cancelled` — cancel `Notify` fired mid-stage. `Cancelled`
    ///   event already emitted; run loop should call
    ///   `finalize_cancelled`.
    ///
    /// SQL failures during the in-helper `update` call surface as
    /// `AppError` via `Result<...>`; the run loop propagates with `?`.
    async fn run_verdict_stage<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        state: JobState,
        profile: &Profile,
        prompt: &str,
        job_id: &str,
        notify: &Notify,
    ) -> Result<VerdictStageOutcome, AppError> {
        let mut stage = match self
            .run_stage_with_cancel(app, state, profile, prompt, job_id, notify)
            .await
        {
            StageOutcome::Ok(s) => s,
            StageOutcome::Err(e) => {
                return Ok(VerdictStageOutcome::InvokeError(e));
            }
            StageOutcome::Cancelled => {
                return Ok(VerdictStageOutcome::Cancelled);
            }
        };

        // Parse the verdict from the assistant text. On parse
        // failure we still append the stage + emit StageCompleted
        // so the W3-14 UI sees the raw text (matches the streaming
        // contract: "the listener observes everything that
        // round-tripped through transport").
        let verdict = match parse_verdict(&stage.assistant_text) {
            Ok(v) => v,
            Err(e) => {
                emit_swarm_event(
                    app,
                    job_id,
                    &SwarmJobEvent::StageCompleted {
                        job_id: job_id.to_string(),
                        stage: stage.clone(),
                    },
                );
                self.registry
                    .update(job_id, |j| {
                        j.stages.push(stage);
                    })
                    .await?;
                return Ok(VerdictStageOutcome::ParseFailed(e));
            }
        };

        // Stamp the parsed verdict onto the StageResult so it lands
        // in `swarm_stages.verdict_json` via the registry's SQL
        // write-through.
        stage.verdict = Some(verdict.clone());

        emit_swarm_event(
            app,
            job_id,
            &SwarmJobEvent::StageCompleted {
                job_id: job_id.to_string(),
                stage: stage.clone(),
            },
        );
        self.registry
            .update(job_id, |j| {
                j.stages.push(stage);
            })
            .await?;

        if verdict.rejected() {
            Ok(VerdictStageOutcome::Rejected(verdict))
        } else {
            Ok(VerdictStageOutcome::Approved(verdict))
        }
    }

    /// Run one FSM stage end-to-end with cancellation support.
    ///
    /// 1. Mark the job's state in the registry.
    /// 2. Emit `StageStarted` (after the state transition lands in
    ///    the registry so observers reading via `JobRegistry::get`
    ///    see the same state the event carries).
    /// 3. Race the `transport.invoke` future against
    ///    `notify.notified()` — whichever resolves first wins.
    /// 4. On the success branch, build a `StageResult` from the
    ///    invoke output. On the cancel branch, emit `Cancelled`
    ///    and return `Cancelled` so the caller can finalize.
    ///
    /// Returns `StageOutcome` rather than `Result<...>` so the
    /// three-way (Ok / Err / Cancelled) dispatch is exhaustively
    /// pattern-matched at the call site — no `Result` widening
    /// hides the cancellation branch.
    async fn run_stage_with_cancel<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        state: JobState,
        profile: &Profile,
        prompt: &str,
        job_id: &str,
        notify: &Notify,
    ) -> StageOutcome {
        if let Err(e) = self
            .registry
            .update(job_id, |j| {
                j.state = state;
            })
            .await
        {
            return StageOutcome::Err(e);
        }
        emit_swarm_event(
            app,
            job_id,
            &SwarmJobEvent::StageStarted {
                job_id: job_id.to_string(),
                state,
                specialist_id: profile.id.clone(),
                prompt_preview: prompt_preview(prompt),
            },
        );

        let started = Instant::now();
        let invoke_fut = self
            .transport
            .invoke(app, profile, prompt, self.stage_timeout);
        // `tokio::select!` polls both branches; the cancel branch
        // wins iff `notify_one()` fires before `invoke_fut`
        // resolves. Dropping `invoke_fut` on the cancel path
        // cancels the pending future — for `SubprocessTransport`
        // this triggers `kill_on_drop(true)` on the child process.
        tokio::select! {
            biased;
            _ = notify.notified() => {
                emit_swarm_event(
                    app,
                    job_id,
                    &SwarmJobEvent::Cancelled {
                        job_id: job_id.to_string(),
                        cancelled_during: state,
                    },
                );
                StageOutcome::Cancelled
            }
            result = invoke_fut => {
                match result {
                    Ok(invoke) => {
                        let duration_ms = started
                            .elapsed()
                            .as_millis()
                            .min(u64::MAX as u128) as u64;
                        StageOutcome::Ok(StageResult {
                            state,
                            specialist_id: profile.id.clone(),
                            assistant_text: invoke.assistant_text,
                            session_id: invoke.session_id,
                            total_cost_usd: invoke.total_cost_usd,
                            duration_ms,
                            // Populated downstream by
                            // `run_verdict_stage` for Review / Test
                            // stages; stays `None` for Scout / Plan /
                            // Build (the run loop calls this helper
                            // directly so the verdict field never
                            // gets touched on those paths).
                            verdict: None,
                            // Populated downstream by the Classify
                            // branch in `run_job_inner` once the
                            // Coordinator output has been parsed
                            // (W3-12f). Every other stage leaves
                            // this `None`.
                            coordinator_decision: None,
                        })
                    }
                    Err(e) => StageOutcome::Err(e),
                }
            }
        }
    }

    /// Mark the job `Failed`, record `last_error`, release the
    /// workspace lock. Used on every error short-circuit so the
    /// happy path's tail block stays unpolluted.
    async fn finalize_failed(
        &self,
        job_id: &str,
        workspace_id: &str,
        last_error: Option<String>,
    ) -> Result<(), AppError> {
        self.registry
            .update(job_id, |j| {
                j.state = JobState::Failed;
                j.last_error = last_error;
            })
            .await?;
        self.registry
            .release_workspace(workspace_id, job_id)
            .await;
        Ok(())
    }

    /// Mirror of `finalize_failed` for verdict-rejected paths
    /// (W3-12d). Sets `state=Failed`, populates `last_verdict`
    /// with the structured Verdict, and clears `last_error` —
    /// the verdict IS the structured error.
    async fn finalize_failed_with_verdict(
        &self,
        job_id: &str,
        workspace_id: &str,
        verdict: Verdict,
    ) -> Result<(), AppError> {
        self.registry
            .update(job_id, |j| {
                j.state = JobState::Failed;
                j.last_error = None;
                j.last_verdict = Some(verdict);
            })
            .await?;
        self.registry
            .release_workspace(workspace_id, job_id)
            .await;
        Ok(())
    }

    /// Mirror of `finalize_failed` for the cancellation path.
    /// Stamps `last_error = "cancelled by user"` so the IPC return
    /// and the `Finished` event both surface a stable, structured
    /// signal that callers can match on. Cancel is a flavor of
    /// failure: `final_state` is `Failed`.
    async fn finalize_cancelled(
        &self,
        job_id: &str,
        workspace_id: &str,
    ) -> Result<(), AppError> {
        tracing::warn!(
            %job_id,
            %workspace_id,
            "swarm job cancelled by user; finalizing as Failed"
        );
        self.registry
            .update(job_id, |j| {
                j.state = JobState::Failed;
                j.last_error = Some(CANCELLED_LAST_ERROR.to_string());
            })
            .await?;
        self.registry
            .release_workspace(workspace_id, job_id)
            .await;
        Ok(())
    }

    /// Build the final `JobOutcome` and emit the trailing
    /// `Finished` event in lock-step. Pulled out as a helper so
    /// every termination path (success, stage error, cancellation)
    /// follows the same emit-then-return shape and an event is
    /// never dropped on a particular branch by accident.
    fn emit_finished_and_build<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        job_id: &str,
    ) -> Result<JobOutcome, AppError> {
        let outcome = self.build_outcome(job_id)?;
        emit_swarm_event(
            app,
            job_id,
            &SwarmJobEvent::Finished {
                job_id: job_id.to_string(),
                outcome: outcome.clone(),
            },
        );
        Ok(outcome)
    }

    /// Aggregate the final `JobOutcome` from the registry record.
    /// Reads-only — the FSM has already finalized state by this
    /// point.
    fn build_outcome(&self, job_id: &str) -> Result<JobOutcome, AppError> {
        let job = self.registry.get(job_id).ok_or_else(|| {
            // The FSM owns the job for its full lifecycle; the
            // registry losing the entry would be a programmer
            // error, not a user-facing condition. Map to Internal
            // so the IPC layer surfaces it as a developer bug.
            AppError::Internal(format!(
                "swarm job `{job_id}` vanished from registry"
            ))
        })?;
        let total_cost_usd: f64 =
            job.stages.iter().map(|s| s.total_cost_usd).sum();
        let total_duration_ms: u64 =
            job.stages.iter().map(|s| s.duration_ms).sum();
        Ok(JobOutcome {
            job_id: job.id,
            final_state: job.state,
            stages: job.stages,
            last_error: job.last_error,
            total_cost_usd,
            total_duration_ms,
            last_verdict: job.last_verdict,
        })
    }
}

/// RAII guard that releases the workspace lock when dropped. Holds
/// a strong `Arc` reference to the registry so the lock can be
/// released even if the original FSM instance has been dropped on
/// an error / panic path.
struct WorkspaceGuard {
    registry: Arc<JobRegistry>,
    workspace_id: String,
    job_id: String,
}

impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        // `release_workspace` is now async (W3-12b) but `Drop` is
        // sync. The happy/error paths in `run_job_inner` always
        // call `release_workspace().await` explicitly before
        // returning, so this Drop is purely a panic-unwind seatbelt.
        // Spawning a one-shot task on the Tauri runtime ensures the
        // in-memory map (and the SQLite lock row, if a pool is
        // wired) gets cleaned up after a panic without forcing
        // every Drop call site to be async. The `release_workspace`
        // method is itself idempotent, so a stale spawn after the
        // explicit release is a no-op.
        let registry = Arc::clone(&self.registry);
        let workspace_id = std::mem::take(&mut self.workspace_id);
        let job_id = std::mem::take(&mut self.job_id);
        tauri::async_runtime::spawn(async move {
            registry.release_workspace(&workspace_id, &job_id).await;
        });
    }
}

/// RAII seatbelt for `JobRegistry::register_cancel`. Mirrors
/// `WorkspaceGuard` so the cancel-notify entry is always removed
/// on scope exit (success, error, panic, cancellation).
/// `unregister_cancel` is idempotent so the FSM tail's explicit
/// removal plus this Drop is safe.
struct CancelGuard {
    registry: Arc<JobRegistry>,
    job_id: String,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.registry.unregister_cancel(&self.job_id);
    }
}

/// Three-way outcome of one stage's race between the invoke future
/// and the cancellation `Notify`. Internal to the FSM; the IPC
/// surface only sees the final `JobOutcome` and the streaming
/// `SwarmJobEvent`s.
enum StageOutcome {
    Ok(StageResult),
    Err(AppError),
    Cancelled,
}

/// Outcome of a verdict-gated stage (Review / Test). Used internally
/// by `run_verdict_stage` so the run loop can pattern-match on five
/// exhaustive cases without unwrapping nested Result types.
///
/// W3-12e refactor: the helper no longer finalizes the job itself on
/// `Rejected` — the run loop owns the retry-vs-finalize choice and
/// needs the parsed Verdict to feed the next Plan prompt. The
/// `Approved` and `Rejected` payloads are the parsed Verdict; the
/// other variants carry just enough context for the run loop to
/// finalize the job (parse-failed text in `last_error`, the
/// underlying `AppError` for stage errors, `Cancelled` for the
/// cancel branch).
enum VerdictStageOutcome {
    /// Verdict.approved=true. FSM advances to the next stage.
    /// `Approved`'s payload IS read on the retry path's
    /// happy-path-after-retry branch (no current consumer, but the
    /// `#[allow(dead_code)]` annotation kept for forward parity).
    Approved(#[allow(dead_code)] Verdict),
    /// Verdict.approved=false. The stage has been appended to
    /// `Job.stages` with the verdict populated. The run loop
    /// decides whether to retry or finalize via
    /// `finalize_failed_with_verdict`.
    Rejected(Verdict),
    /// The assistant_text could not be parsed as a Verdict. The
    /// stage IS appended (with `verdict=None`) so the W3-14 UI sees
    /// the raw text. The run loop finalizes via `finalize_failed`
    /// with the parse error in `last_error`.
    ParseFailed(AppError),
    /// `transport.invoke` returned an error before any verdict
    /// could be parsed. No stage is appended. The run loop
    /// finalizes via `finalize_failed`.
    InvokeError(AppError),
    /// Cancellation `Notify` fired before the stage could complete.
    /// `Cancelled` event has already been emitted. The run loop
    /// finalizes via `finalize_cancelled`.
    Cancelled,
}

/// Stable last-error string the FSM writes when cancelled. Pulled
/// out as a `const` so tests can match on it without the cancel
/// path's wording drifting.
pub(crate) const CANCELLED_LAST_ERROR: &str = "cancelled by user";

/// First 200 *chars* of the rendered prompt — bounded by character
/// count (not bytes) so multi-byte Turkish text is never split
/// mid-codepoint. Returned as an owned `String` because the event
/// payload outlives the prompt slice.
fn prompt_preview(prompt: &str) -> String {
    prompt.chars().take(200).collect()
}

/// Emit one `SwarmJobEvent` on the `swarm:job:{job_id}:event`
/// channel. Errors from `app.emit` are swallowed with a structured
/// warning — the FSM never aborts because a Tauri event failed to
/// dispatch (e.g. the window is closing during a transition). The
/// IPC return value remains the authoritative source of truth.
fn emit_swarm_event<R: Runtime>(
    app: &AppHandle<R>,
    job_id: &str,
    event: &SwarmJobEvent,
) {
    let event_name = events::swarm_job_event(job_id);
    if let Err(e) = app.emit(&event_name, event) {
        tracing::warn!(
            event_name = %event_name,
            error = %e,
            "swarm event emit failed; continuing FSM"
        );
    }
}

/// Unix epoch milliseconds — wraps the `SystemTime` boilerplate so
/// the FSM constructor reads as one line. Returns 0 on the
/// (impossible-in-practice) clock-before-epoch case rather than
/// panicking; the timestamp is informational only.
fn current_unix_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::profile::{PermissionMode, ProfileRegistry};
    use crate::swarm::transport::mock_transport::{
        MockResponse, MockTransport,
    };
    use crate::swarm::transport::InvokeResult;
    use crate::test_support::mock_app_with_pool;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn ok_response(text: &str, cost: f64) -> MockResponse {
        MockResponse {
            result: Ok(InvokeResult {
                session_id: format!("sess-{}", text.chars().take(4).collect::<String>()),
                assistant_text: text.to_string(),
                total_cost_usd: cost,
                turn_count: 1,
            }),
            sleep: None,
        }
    }

    fn err_response(reason: &str) -> MockResponse {
        MockResponse {
            result: Err(AppError::SwarmInvoke(reason.to_string())),
            sleep: None,
        }
    }

    /// Canned MockResponse whose `assistant_text` is a valid
    /// Verdict JSON with `approved=true`. Reviewers + Testers in
    /// the happy-path tests use this to skip past the verdict gate
    /// without forcing every test to spell out the JSON inline.
    fn ok_verdict_response(cost: f64) -> MockResponse {
        MockResponse {
            result: Ok(InvokeResult {
                session_id: "sess-verdict".into(),
                assistant_text: r#"{"approved":true,"issues":[],"summary":"OK"}"#.into(),
                total_cost_usd: cost,
                turn_count: 1,
            }),
            sleep: None,
        }
    }

    /// Canned MockResponse whose `assistant_text` is a valid
    /// `CoordinatorDecision` JSON with `route=execute_plan` and
    /// `scope=backend`. The W3-12f Coordinator brain runs once per
    /// job between Scout and Plan; the default mock keeps the
    /// existing 5-stage chain intact by always voting ExecutePlan.
    /// W3-12g added the required `scope` field.
    fn execute_plan_decision_response() -> MockResponse {
        MockResponse {
            result: Ok(InvokeResult {
                session_id: "sess-coord".into(),
                assistant_text:
                    r#"{"route":"execute_plan","scope":"backend","reasoning":"mock"}"#.into(),
                total_cost_usd: 0.0,
                turn_count: 1,
            }),
            sleep: None,
        }
    }

    /// Canned MockResponse whose `assistant_text` is a valid
    /// `CoordinatorDecision` JSON with `route=research_only` and
    /// `scope=backend`. Used by W3-12f tests that exercise the
    /// short-circuit branch. W3-12g added the required `scope` field.
    fn research_only_decision_response() -> MockResponse {
        MockResponse {
            result: Ok(InvokeResult {
                session_id: "sess-coord-r".into(),
                assistant_text:
                    r#"{"route":"research_only","scope":"backend","reasoning":"mock"}"#.into(),
                total_cost_usd: 0.0,
                turn_count: 1,
            }),
            sleep: None,
        }
    }

    /// Canned MockResponse whose `assistant_text` is a valid
    /// `CoordinatorDecision` JSON with `route=execute_plan` and
    /// `scope=frontend`. W3-12g introduced this for scope-emit
    /// tests; W3-12h's scope-aware dispatch tests reuse it to
    /// drive the frontend chain (FrontendBuilder + FrontendReviewer).
    fn execute_plan_frontend_decision_response() -> MockResponse {
        MockResponse {
            result: Ok(InvokeResult {
                session_id: "sess-coord-fe".into(),
                assistant_text:
                    r#"{"route":"execute_plan","scope":"frontend","reasoning":"frontend goal"}"#.into(),
                total_cost_usd: 0.0,
                turn_count: 1,
            }),
            sleep: None,
        }
    }

    /// Canned MockResponse whose `assistant_text` is a valid
    /// `CoordinatorDecision` JSON with `route=execute_plan` and
    /// `scope=fullstack`. W3-12h uses this to verify the FSM
    /// falls back to the backend chain (Fullstack sequential
    /// dispatch lands in W3-12i).
    fn execute_plan_fullstack_decision_response() -> MockResponse {
        MockResponse {
            result: Ok(InvokeResult {
                session_id: "sess-coord-fs".into(),
                assistant_text:
                    r#"{"route":"execute_plan","scope":"fullstack","reasoning":"fullstack goal"}"#.into(),
                total_cost_usd: 0.0,
                turn_count: 1,
            }),
            sleep: None,
        }
    }

    /// Canned MockResponse whose `assistant_text` is a valid
    /// Verdict JSON with `approved=false` and one high-severity
    /// issue. Used by the verdict-rejection FSM tests.
    fn rejected_verdict_response(issue_msg: &str) -> MockResponse {
        let payload = format!(
            r#"{{"approved":false,"issues":[{{"severity":"high","msg":"{}"}}],"summary":"rejected"}}"#,
            issue_msg.replace('\\', "\\\\").replace('"', "\\\"")
        );
        MockResponse {
            result: Ok(InvokeResult {
                session_id: "sess-rej".into(),
                assistant_text: payload,
                total_cost_usd: 0.0,
                turn_count: 1,
            }),
            sleep: None,
        }
    }

    /// Build the canonical 5-stage happy-path response set:
    /// scout / planner / builder return `ok_response(text, cost)`;
    /// reviewer + tester return approved verdicts. Used by tests
    /// that assert on the full chain reaching Done.
    ///
    /// W3-12f: the FSM now runs a Classify stage between Scout and
    /// Plan, so the response set also seeds the Coordinator with an
    /// ExecutePlan decision — this preserves the existing 5-stage
    /// flow in tests that do not care about the brain branch
    /// (stages.len() becomes 6 because Classify is appended).
    fn happy_5_stage_responses(
        scout_text: &str,
        plan_text: &str,
        build_text: &str,
        scout_cost: f64,
        plan_cost: f64,
        build_cost: f64,
    ) -> HashMap<String, MockResponse> {
        let mut r: HashMap<String, MockResponse> = HashMap::new();
        r.insert(SCOUT_ID.into(), ok_response(scout_text, scout_cost));
        r.insert(PLANNER_ID.into(), ok_response(plan_text, plan_cost));
        r.insert(BACKEND_BUILDER_ID.into(), ok_response(build_text, build_cost));
        r.insert(BACKEND_REVIEWER_ID.into(), ok_verdict_response(0.0));
        r.insert(TESTER_ID.into(), ok_verdict_response(0.0));
        r.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        r
    }

    /// Build a registry holding the bundled profiles. `load_from(None)`
    /// reads the embedded scout/planner/backend-builder set so we
    /// don't have to hand-roll fixture files.
    fn bundled_registry() -> Arc<ProfileRegistry> {
        Arc::new(
            ProfileRegistry::load_from(None).expect("bundled registry"),
        )
    }

    /// A minimal hand-rolled registry with three throwaway profiles
    /// that share ids with the bundled set. Used by tests that don't
    /// want to fish persona text out of the embedded `.md` files.
    fn synthetic_registry() -> Arc<ProfileRegistry> {
        // Reuse the bundled registry — the FSM only reads `Profile.id`
        // and forwards `body` to the transport, which is mocked. No
        // need for synthetic profiles.
        bundled_registry()
    }

    /// Mock-driven happy path: all six stages OK (W3-12f layered
    /// Classify between Scout and Plan); reviewer + tester emit
    /// `approved=true` verdicts; coordinator votes ExecutePlan.
    #[tokio::test]
    async fn fsm_walks_five_stages_on_approved_path() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let responses = happy_5_stage_responses(
            "scout findings",
            "plan steps",
            "build done",
            0.01,
            0.02,
            0.03,
        );
        let mock = MockTransport::new(responses);

        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            mock,
            Arc::clone(&registry),
            Duration::from_secs(5),
        );

        let outcome = fsm
            .run_job(app.handle(), "ws-happy".into(), "do thing".into())
            .await
            .expect("happy path returns Ok");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 6);
        assert!(outcome.last_error.is_none());
        assert!(outcome.last_verdict.is_none());
        assert!(outcome.total_cost_usd > 0.0);
        assert!(
            (outcome.total_cost_usd - 0.06).abs() < 1e-9,
            "cost sum off (scout+plan+build only since verdicts/coordinator free): {}",
            outcome.total_cost_usd
        );
        // Stage state ordering matches the FSM's fixed sequence
        // post-W3-12f: Scout → Classify → Plan → Build → Review → Test.
        assert_eq!(outcome.stages[0].state, JobState::Scout);
        assert_eq!(outcome.stages[1].state, JobState::Classify);
        assert_eq!(outcome.stages[2].state, JobState::Plan);
        assert_eq!(outcome.stages[3].state, JobState::Build);
        assert_eq!(outcome.stages[4].state, JobState::Review);
        assert_eq!(outcome.stages[5].state, JobState::Test);
        // Verdict populated on Review/Test, None on the others.
        assert!(outcome.stages[0].verdict.is_none());
        assert!(outcome.stages[1].verdict.is_none());
        assert!(outcome.stages[2].verdict.is_none());
        assert!(outcome.stages[3].verdict.is_none());
        assert!(
            outcome.stages[4]
                .verdict
                .as_ref()
                .map(|v| v.approved)
                .unwrap_or(false),
            "review verdict should be approved"
        );
        assert!(
            outcome.stages[5]
                .verdict
                .as_ref()
                .map(|v| v.approved)
                .unwrap_or(false),
            "test verdict should be approved"
        );
        // Coordinator decision lands on the Classify stage; every
        // other stage leaves it None.
        assert!(outcome.stages[1].coordinator_decision.is_some());
        assert_eq!(
            outcome.stages[1]
                .coordinator_decision
                .as_ref()
                .map(|d| d.route),
            Some(CoordinatorRoute::ExecutePlan)
        );

        // Workspace lock released — second job on same workspace
        // succeeds.
        let responses2 = happy_5_stage_responses(
            "s2", "p2", "b2", 0.01, 0.01, 0.01,
        );
        let fsm2 = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses2),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        fsm2.run_job(app.handle(), "ws-happy".into(), "again".into())
            .await
            .expect("workspace lock was released");
    }

    /// Scout failure → no stages recorded, Failed state, error in
    /// `last_error`.
    #[tokio::test]
    async fn fsm_scout_failure_short_circuits() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), err_response("scout boom"));
        responses
            .insert(PLANNER_ID.into(), ok_response("unused", 0.0));
        responses
            .insert(BACKEND_BUILDER_ID.into(), ok_response("unused", 0.0));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-1".into(), "x".into())
            .await
            .expect("FSM returns Ok with Failed outcome");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert!(outcome.stages.is_empty(), "no stages on scout fail");
        assert!(outcome
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("scout boom"));
    }

    /// Planner failure → scout + classify stages recorded, then Failed.
    /// W3-12f: classify lands between scout and plan; planner is the
    /// fourth dispatch and the failure surface still records 2
    /// successful stages (scout + classify).
    #[tokio::test]
    async fn fsm_planner_failure_short_circuits() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses
            .insert(SCOUT_ID.into(), ok_response("scout ok", 0.01));
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        responses.insert(PLANNER_ID.into(), err_response("planner boom"));
        responses
            .insert(BACKEND_BUILDER_ID.into(), ok_response("unused", 0.0));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-2".into(), "x".into())
            .await
            .expect("FSM returns Ok with Failed outcome");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert_eq!(outcome.stages.len(), 2);
        assert_eq!(outcome.stages[0].state, JobState::Scout);
        assert_eq!(outcome.stages[1].state, JobState::Classify);
        assert!(outcome
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("planner boom"));
    }

    /// Builder failure → scout + classify + planner stages recorded,
    /// then Failed.
    /// W3-12f: classify lands between scout and plan, so the partial
    /// stage count rises from 2 to 3.
    #[tokio::test]
    async fn fsm_builder_failure_returns_partial_stages() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses
            .insert(SCOUT_ID.into(), ok_response("scout ok", 0.01));
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        responses
            .insert(PLANNER_ID.into(), ok_response("plan ok", 0.02));
        responses.insert(BACKEND_BUILDER_ID.into(), err_response("builder boom"));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-3".into(), "x".into())
            .await
            .expect("FSM returns Ok with Failed outcome");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert_eq!(outcome.stages.len(), 3);
        assert_eq!(outcome.stages[0].state, JobState::Scout);
        assert_eq!(outcome.stages[1].state, JobState::Classify);
        assert_eq!(outcome.stages[2].state, JobState::Plan);
        assert!(outcome
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("builder boom"));
    }

    /// Total cost aggregates across stages on the happy path.
    /// Reviewer + tester verdicts are zero-cost so the assertion
    /// stays at 0.06.
    #[tokio::test]
    async fn fsm_aggregates_total_cost() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let responses = happy_5_stage_responses(
            "a", "b", "c", 0.01, 0.02, 0.03,
        );
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-cost".into(), "g".into())
            .await
            .expect("ok");
        assert!((outcome.total_cost_usd - 0.06).abs() < 0.001);
    }

    /// Scout receives the goal wrapped in an investigation-shaped
    /// template (post-2026-05-05 fix; the verbatim variant burned
    /// Scout's max_turns budget when the goal was a "do X" task).
    #[tokio::test]
    async fn prompt_template_scout_wraps_goal_as_investigation() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses = happy_5_stage_responses(
            "X", "Y", "Z", 0.0, 0.0, 0.0,
        );
        // Override scout/plan/build to the canonical strings the
        // assertions look at; verdicts + coordinator come from the helper.
        responses.insert(SCOUT_ID.into(), ok_response("X", 0.0));
        responses.insert(PLANNER_ID.into(), ok_response("Y", 0.0));
        responses.insert(BACKEND_BUILDER_ID.into(), ok_response("Z", 0.0));
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        // Build the mock with a holder so we can read `seen()` after
        // run_job — `MockTransport` only exposes `seen()` through
        // `&self`, so we keep a raw reference via Arc.
        let mock = Arc::new(MockTransport::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcTransport(Arc::clone(&mock)),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let goal = "exactly-this-goal-string";
        fsm.run_job(app.handle(), "ws-pt-scout".into(), goal.into())
            .await
            .expect("ok");
        let seen = mock.seen();
        let scout_prompt = seen
            .iter()
            .find(|(id, _)| id == SCOUT_ID)
            .map(|(_, p)| p.as_str())
            .expect("scout prompt recorded");
        // Goal is preserved inside the template; no longer verbatim.
        assert!(
            scout_prompt.contains(goal),
            "scout prompt should contain the goal; got: {scout_prompt}"
        );
        // Investigation framing must be present so Scout's persona
        // does not interpret a "do X" goal as a write directive.
        assert!(
            scout_prompt.contains("KOD YAZMIYORSUN"),
            "scout prompt should restate the read-only contract; got: {scout_prompt}"
        );
    }

    /// Planner prompt contains the scout's assistant text.
    #[tokio::test]
    async fn prompt_template_plan_includes_scout_findings() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses = happy_5_stage_responses(
            "scout-discovered-finding-XYZ",
            "plan",
            "build",
            0.0,
            0.0,
            0.0,
        );
        responses.insert(
            SCOUT_ID.into(),
            ok_response("scout-discovered-finding-XYZ", 0.0),
        );
        responses.insert(PLANNER_ID.into(), ok_response("plan", 0.0));
        responses.insert(BACKEND_BUILDER_ID.into(), ok_response("build", 0.0));
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        let mock = Arc::new(MockTransport::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcTransport(Arc::clone(&mock)),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        fsm.run_job(app.handle(), "ws-pt-plan".into(), "G".into())
            .await
            .expect("ok");
        let seen = mock.seen();
        let plan_prompt = seen
            .iter()
            .find(|(id, _)| id == PLANNER_ID)
            .map(|(_, p)| p.as_str())
            .expect("planner prompt recorded");
        assert!(
            plan_prompt.contains("scout-discovered-finding-XYZ"),
            "plan prompt missing scout findings: {plan_prompt}"
        );
        assert!(
            plan_prompt.contains("Hedef:"),
            "plan prompt should carry the Turkish template header"
        );
    }

    /// Builder prompt contains the step-1 directive (Turkish).
    #[tokio::test]
    async fn prompt_template_build_includes_plan_step1_directive() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses = happy_5_stage_responses(
            "s", "plan-text", "b", 0.0, 0.0, 0.0,
        );
        responses.insert(SCOUT_ID.into(), ok_response("s", 0.0));
        responses
            .insert(PLANNER_ID.into(), ok_response("plan-text", 0.0));
        responses.insert(BACKEND_BUILDER_ID.into(), ok_response("b", 0.0));
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        let mock = Arc::new(MockTransport::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcTransport(Arc::clone(&mock)),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        fsm.run_job(app.handle(), "ws-pt-build".into(), "G".into())
            .await
            .expect("ok");
        let seen = mock.seen();
        let build_prompt = seen
            .iter()
            .find(|(id, _)| id == BACKEND_BUILDER_ID)
            .map(|(_, p)| p.as_str())
            .expect("build prompt recorded");
        assert!(
            build_prompt.contains("ŞU ANDA SADECE ADIM 1'İ UYGULA"),
            "build prompt missing step-1 directive: {build_prompt}"
        );
        assert!(
            build_prompt.contains("plan-text"),
            "build prompt should embed the planner output"
        );
    }

    /// Per-stage duration is measured around the invoke await.
    /// Mock injects a 50ms sleep per stage; assert each
    /// `StageResult.duration_ms >= 50`.
    /// W3-12f: 6 stages now (Scout, Classify, Plan, Build, Review, Test).
    #[tokio::test]
    async fn fsm_records_per_stage_duration() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        let sleep = Some(Duration::from_millis(50));
        for (id, sess, text) in [
            (SCOUT_ID, "s1", "scout"),
            (PLANNER_ID, "s2", "plan"),
            (BACKEND_BUILDER_ID, "s3", "build"),
        ] {
            responses.insert(
                id.into(),
                MockResponse {
                    result: Ok(InvokeResult {
                        session_id: sess.into(),
                        assistant_text: text.into(),
                        total_cost_usd: 0.01,
                        turn_count: 1,
                    }),
                    sleep,
                },
            );
        }
        // Reviewer + tester also sleep 50ms each so their
        // duration_ms entries clear the per-stage threshold.
        let verdict_text =
            r#"{"approved":true,"issues":[],"summary":"OK"}"#;
        for (id, sess) in [
            (BACKEND_REVIEWER_ID, "s4"),
            (TESTER_ID, "s5"),
        ] {
            responses.insert(
                id.into(),
                MockResponse {
                    result: Ok(InvokeResult {
                        session_id: sess.into(),
                        assistant_text: verdict_text.into(),
                        total_cost_usd: 0.0,
                        turn_count: 1,
                    }),
                    sleep,
                },
            );
        }
        // Coordinator brain (W3-12f) — sleep 50ms so its duration_ms
        // also clears the threshold.
        responses.insert(
            COORDINATOR_ID.into(),
            MockResponse {
                result: Ok(InvokeResult {
                    session_id: "s-coord".into(),
                    assistant_text:
                        r#"{"route":"execute_plan","scope":"backend","reasoning":"mock"}"#.into(),
                    total_cost_usd: 0.0,
                    turn_count: 1,
                }),
                sleep,
            },
        );
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-dur".into(), "g".into())
            .await
            .expect("ok");
        assert_eq!(outcome.stages.len(), 6);
        for stage in &outcome.stages {
            assert!(
                stage.duration_ms >= 50,
                "stage {:?} duration_ms = {}",
                stage.state,
                stage.duration_ms
            );
        }
        // Total duration is at least 6*50=300ms.
        assert!(outcome.total_duration_ms >= 300);
    }

    /// W3-12d activated Review/Test in `next_state`. Both the OK
    /// and Err transitions are now defined; the verdict-driven
    /// gate is layered on top by the run loop.
    #[test]
    fn next_state_full_table_review_and_test() {
        assert_eq!(next_state(JobState::Build, true), JobState::Review);
        assert_eq!(next_state(JobState::Build, false), JobState::Failed);
        assert_eq!(next_state(JobState::Review, true), JobState::Test);
        assert_eq!(next_state(JobState::Review, false), JobState::Failed);
        assert_eq!(next_state(JobState::Test, true), JobState::Done);
        assert_eq!(next_state(JobState::Test, false), JobState::Failed);
    }

    /// Empty workspace_id is rejected before any registry mutation.
    #[tokio::test]
    async fn run_job_rejects_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(HashMap::new()),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let err = fsm
            .run_job(app.handle(), "".into(), "x".into())
            .await
            .expect_err("empty workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
        // No job landed in the registry.
        assert!(registry.list().is_empty());
    }

    /// Empty goal is rejected the same way.
    #[tokio::test]
    async fn run_job_rejects_empty_goal() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(HashMap::new()),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let err = fsm
            .run_job(app.handle(), "ws".into(), "   ".into())
            .await
            .expect_err("empty goal rejected");
        assert_eq!(err.kind(), "invalid_input");
        assert!(registry.list().is_empty());
    }

    /// Integration smoke — same shape as W3-11's
    /// `integration_smoke_invoke` but walks the full FSM chain
    /// against the real `claude` binary. CI lacks `claude` + an
    /// OAuth session so the test is `#[ignore]`d; the owner runs
    /// it manually with `cargo test -- --ignored` post-commit.
    ///
    /// Time budget: 3 × 180s = 540s worst-case. The 180s/stage
    /// default is generous because Windows + antivirus cold-cache
    /// first-spawn of `claude.cmd` can spend 30–60s on AV alone
    /// (observed 2026-05-05 on owner's machine — 60s/stage caused
    /// a stage timeout on Builder). Tighten via
    /// `NEURON_SWARM_STAGE_TIMEOUT_SEC=<sec>` for fast-machine
    /// runs.
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_fsm_drives_real_claude_chain() {
        use crate::swarm::transport::SubprocessTransport;

        let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(180);

        let (app, _pool, _dir) = mock_app_with_pool().await;
        let profiles = bundled_registry();
        let transport = SubprocessTransport::new();
        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            profiles,
            transport,
            registry,
            Duration::from_secs(stage_secs),
        );
        // Goal kept minimal so Builder fits inside backend-builder
        // profile's max_turns=12 budget. Path-free so it's CWD-
        // agnostic (test may run from repo root or from src-tauri/).
        // Earlier richer goals ("plus a unit test") hit
        // `error_max_turns` on Windows + AV cold-cache (2026-05-05).
        let goal = "Find the `impl ProfileRegistry` block in \
            profile.rs and add a one-line public method \
            `pub fn profile_count(&self) -> usize { self.profiles.len() }` \
            right after the existing `list` method. Just the method. \
            Do NOT add a unit test, do NOT add doc comments, do NOT \
            run cargo check.";
        let outcome = fsm
            .run_job(app.handle(), "default".into(), goal.into())
            .await
            .expect("FSM returns Ok");
        assert_eq!(
            outcome.final_state,
            JobState::Done,
            "expected Done, got {:?} (last_error: {:?})",
            outcome.final_state,
            outcome.last_error
        );
        // W3-12f: 6 stages on the ExecutePlan branch (scout / classify
        // / plan / build / review / test).
        assert_eq!(outcome.stages.len(), 6);
    }

    // ----------------------------------------------------------------
    // Adapter — lets a `&Arc<MockTransport>` satisfy `T: Transport`
    // so the prompt-template tests can keep a handle on the mock for
    // post-run `seen()` inspection while still passing it by value
    // to `CoordinatorFsm::new`. Defined inside the test module so
    // it never leaks into release artifacts.
    // ----------------------------------------------------------------

    struct ArcTransport(Arc<MockTransport>);

    impl Transport for ArcTransport {
        async fn invoke<R: Runtime>(
            &self,
            app: &AppHandle<R>,
            profile: &Profile,
            user_message: &str,
            timeout: Duration,
        ) -> Result<InvokeResult, AppError> {
            self.0.invoke(app, profile, user_message, timeout).await
        }
    }

    /// Integration smoke (`#[ignore]`) — WP-W3-12b acceptance.
    /// Drives the W3-12a happy-path goal against a SQLite-backed
    /// registry and asserts the persisted footprint after
    /// completion: 1 Done job, 3 stage rows, 0 workspace_locks.
    /// CI lacks the binary so this stays opt-in via
    /// `cargo test -- --ignored`. Owner runs it manually post-commit.
    ///
    /// Time budget: 3 × 180s = 540s worst-case (matches the W3-12a
    /// integration smoke).
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_persistence_survives_real_claude_chain() {
        use crate::swarm::transport::SubprocessTransport;

        let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(180);

        let (app, _pool, _dir) = mock_app_with_pool().await;
        let (test_pool, _pool_dir) =
            crate::test_support::fresh_pool().await;
        let profiles = bundled_registry();
        let transport = SubprocessTransport::new();
        let registry =
            Arc::new(JobRegistry::with_pool(test_pool.clone()));
        let fsm = CoordinatorFsm::new(
            profiles,
            transport,
            Arc::clone(&registry),
            Duration::from_secs(stage_secs),
        );
        // Reuse the canonical W3-12a goal. Path-free / CWD-agnostic.
        let goal = "Find the `impl ProfileRegistry` block in \
            profile.rs and add a one-line public method \
            `pub fn profile_count(&self) -> usize { self.profiles.len() }` \
            right after the existing `list` method. Just the method. \
            Do NOT add a unit test, do NOT add doc comments, do NOT \
            run cargo check.";
        let outcome = fsm
            .run_job(app.handle(), "default".into(), goal.into())
            .await
            .expect("FSM returns Ok");
        assert_eq!(outcome.final_state, JobState::Done);

        // Persisted footprint matches the WP §"acceptance gate":
        // 1 Done job + 3 stage rows + 0 workspace_locks.
        let done_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_jobs WHERE state = 'done'",
        )
        .fetch_one(&test_pool)
        .await
        .expect("count done");
        assert_eq!(done_count, 1, "exactly one Done job persisted");
        let stage_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM swarm_stages")
                .fetch_one(&test_pool)
                .await
                .expect("count stages");
        // W3-12f: 6 stage rows on the ExecutePlan happy path
        // (scout / classify / plan / build / review / test).
        assert_eq!(stage_count, 6, "six stage rows persisted");
        let lock_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_workspace_locks",
        )
        .fetch_one(&test_pool)
        .await
        .expect("count locks");
        assert_eq!(lock_count, 0, "workspace lock cleared on Done");
    }

    /// Integration smoke (`#[ignore]`) — drives the real `claude`
    /// chain and signals a cancel after the first `StageStarted`
    /// event lands. CI lacks the binary and an OAuth session so
    /// this stays opt-in via `cargo test -- --ignored`. Owner runs
    /// it manually post-commit.
    ///
    /// Time budget: 180s/stage default (matches
    /// `integration_fsm_drives_real_claude_chain`); typical wall
    /// clock for cancel-during-build is well under 60s because
    /// Scout + Plan finish fast and the cancel lands mid-Build.
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_cancel_during_real_claude_chain() {
        use crate::swarm::transport::SubprocessTransport;
        use std::sync::Mutex as StdMutex;
        use tauri::Listener;

        let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(180);

        let (app, _pool, _dir) = mock_app_with_pool().await;
        let profiles = bundled_registry();
        let transport = SubprocessTransport::new();
        let registry = Arc::new(JobRegistry::new());
        let fsm = Arc::new(CoordinatorFsm::new(
            profiles,
            transport,
            Arc::clone(&registry),
            Duration::from_secs(stage_secs),
        ));

        // Pre-register a known job_id and subscribe before run.
        let job_id = "j-integration-cancel".to_string();
        let captured: Arc<StdMutex<Vec<CapturedEvent>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let captured_w = Arc::clone(&captured);
        let event_name = crate::events::swarm_job_event(&job_id);
        app.listen(event_name, move |event| {
            let payload = event.payload().to_string();
            if let Ok(value) =
                serde_json::from_str::<serde_json::Value>(&payload)
            {
                let kind = value
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                captured_w
                    .lock()
                    .expect("captured events lock poisoned")
                    .push(CapturedEvent { kind, json: value });
            }
        });

        // Spawn the FSM. Use the canonical no-op goal from the
        // existing happy-path integration test.
        let app_handle = app.handle().clone();
        let fsm_for_task = Arc::clone(&fsm);
        let job_id_for_task = job_id.clone();
        let goal = "Find the `impl ProfileRegistry` block in \
            profile.rs and add a one-line public method \
            `pub fn profile_count(&self) -> usize { self.profiles.len() }` \
            right after the existing `list` method. Just the method. \
            Do NOT add a unit test, do NOT add doc comments, do NOT \
            run cargo check.";
        let handle = tokio::spawn(async move {
            fsm_for_task
                .run_job_with_id(
                    &app_handle,
                    job_id_for_task,
                    "default".into(),
                    goal.into(),
                )
                .await
        });

        // Wait for the first StageStarted with state == build (or
        // any later stage). Race-tolerant: cancel may land during
        // Plan if Build doesn't start within the timeout.
        let observed = wait_for_any_stage_started(
            &captured,
            Duration::from_secs(stage_secs * 2),
        )
        .await;
        tracing::info!(?observed, "cancelling integration FSM");

        registry.signal_cancel(&job_id).expect("signal cancel ok");

        let outcome = handle
            .await
            .expect("task ok")
            .expect("FSM returns Ok");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert_eq!(
            outcome.last_error.as_deref(),
            Some(CANCELLED_LAST_ERROR)
        );
        // Recorded events must contain a Cancelled event.
        let events = captured
            .lock()
            .expect("captured events lock poisoned")
            .clone();
        let cancelled = events
            .iter()
            .find(|e| e.kind == KIND_CANCELLED)
            .expect("Cancelled event captured");
        let cancelled_during = cancelled
            .json
            .get("cancelled_during")
            .and_then(|v| v.as_str())
            .expect("cancelled_during str");
        // Race-tolerant: cancel landed in Scout / Plan / Build.
        assert!(
            ["scout", "plan", "build"].contains(&cancelled_during),
            "unexpected cancelled_during: {cancelled_during}"
        );
    }

    /// Wait until at least one StageStarted event has landed in the
    /// captured stream. Returns the wire-form state the event
    /// carried so the caller can log which stage triggered.
    async fn wait_for_any_stage_started(
        events: &Arc<std::sync::Mutex<Vec<CapturedEvent>>>,
        timeout: Duration,
    ) -> String {
        let start = Instant::now();
        loop {
            {
                let guard = events
                    .lock()
                    .expect("captured events lock poisoned");
                if let Some(e) =
                    guard.iter().find(|e| e.kind == KIND_STAGE_STARTED)
                {
                    return e
                        .json
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                }
            }
            if start.elapsed() > timeout {
                panic!(
                    "no StageStarted observed within {timeout:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // ----------------------------------------------------------------
    // WP-W3-12b — parameterized regressions against a SQLite-backed
    // registry. Picks 3 representative behaviors so the SQL path is
    // covered without doubling the FSM test surface; the dedicated
    // store tests cover the per-op SQL semantics.
    // ----------------------------------------------------------------

    /// Build a SQLite-backed registry by chaining the test pool
    /// helper. Returns the registry + the pool's owning TempDir so
    /// callers can keep the file alive for the test's lifetime.
    async fn pool_backed_registry() -> (
        Arc<JobRegistry>,
        crate::db::DbPool,
        tempfile::TempDir,
    ) {
        let (pool, dir) = crate::test_support::fresh_pool().await;
        (
            Arc::new(JobRegistry::with_pool(pool.clone())),
            pool,
            dir,
        )
    }

    /// Pool-backed happy path: the FSM walks all six stages
    /// (Scout/Classify/Plan/Build/Review/Test) against a
    /// SQLite-backed registry; on completion the `swarm_jobs` row
    /// is `done`, six `swarm_stages` rows, no `swarm_workspace_locks`.
    #[tokio::test]
    async fn fsm_happy_path_walks_five_stages_with_pool() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let (registry, pool, _reg_dir) = pool_backed_registry().await;
        let responses = happy_5_stage_responses(
            "scout findings",
            "plan steps",
            "build done",
            0.01,
            0.02,
            0.03,
        );
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-pool-happy".into(), "g".into())
            .await
            .expect("ok");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 6);

        let job_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM swarm_jobs WHERE state = 'done'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(job_count, 1);
        let stage_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM swarm_stages")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stage_count, 6);
        // The Review and Test stage rows carry verdict_json; the
        // others are NULL (Scout/Classify/Plan/Build never run a verdict).
        let verdict_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_stages WHERE verdict_json IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(verdict_rows, 2, "review + test stages carry verdict_json");
        // The Classify stage row carries decision_json; every other
        // row stays NULL.
        let decision_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_stages WHERE decision_json IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(decision_rows, 1, "classify stage carries decision_json");
        let lock_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_workspace_locks",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(lock_count, 0, "lock released after Done");
    }

    /// Pool-backed scout failure: zero stage rows, job row
    /// `failed`, last_error matches the boom string, lock cleared.
    #[tokio::test]
    async fn fsm_scout_failure_short_circuits_with_pool() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let (registry, pool, _reg_dir) = pool_backed_registry().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), err_response("scout boom"));
        responses.insert(PLANNER_ID.into(), ok_response("u", 0.0));
        responses.insert(BACKEND_BUILDER_ID.into(), ok_response("u", 0.0));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-pool-fail".into(), "g".into())
            .await
            .expect("ok");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert!(outcome.stages.is_empty());

        let last_error: Option<String> = sqlx::query_scalar(
            "SELECT last_error FROM swarm_jobs WHERE workspace_id = ?",
        )
        .bind("ws-pool-fail")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(last_error
            .as_deref()
            .unwrap_or("")
            .contains("scout boom"));
        let stage_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM swarm_stages")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stage_count, 0);
        let lock_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_workspace_locks",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(lock_count, 0);
    }

    /// Pool-backed planner failure: scout + classify stage rows
    /// persist, plan/build do not. Final job state is `failed`,
    /// lock cleared. W3-12f: classify lands between scout and plan,
    /// so the partial stage count rises from 1 to 2.
    #[tokio::test]
    async fn fsm_planner_failure_persists_partial_stages_with_pool() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let (registry, pool, _reg_dir) = pool_backed_registry().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), ok_response("scout", 0.01));
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        responses.insert(PLANNER_ID.into(), err_response("plan boom"));
        responses.insert(BACKEND_BUILDER_ID.into(), ok_response("u", 0.0));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-pool-plan".into(), "g".into())
            .await
            .expect("ok");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert_eq!(outcome.stages.len(), 2);

        let stage_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM swarm_stages")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stage_count, 2, "scout + classify stage rows persisted");
        let stage_state: String = sqlx::query_scalar(
            "SELECT state FROM swarm_stages WHERE idx = 0",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stage_state, "scout");
        let classify_state: String = sqlx::query_scalar(
            "SELECT state FROM swarm_stages WHERE idx = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(classify_state, "classify");
        let lock_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_workspace_locks",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(lock_count, 0);
    }

    /// Pool-backed cancel-during-build: cancel mid-flight, the job
    /// row is `failed` with the canonical `cancelled by user`
    /// last_error and the lock row is gone.
    #[tokio::test]
    async fn fsm_cancel_during_build_persists_failed_with_pool() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let (registry, pool, _reg_dir) = pool_backed_registry().await;
        // Make build slow so cancel can land mid-stage.
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        responses.insert(SCOUT_ID.into(), MockResponse {
            result: Ok(InvokeResult {
                session_id: "s".into(),
                assistant_text: "scout".into(),
                total_cost_usd: 0.01,
                turn_count: 1,
            }),
            sleep: Some(Duration::from_millis(10)),
        });
        responses.insert(PLANNER_ID.into(), MockResponse {
            result: Ok(InvokeResult {
                session_id: "p".into(),
                assistant_text: "plan".into(),
                total_cost_usd: 0.01,
                turn_count: 1,
            }),
            sleep: Some(Duration::from_millis(10)),
        });
        responses.insert(BACKEND_BUILDER_ID.into(), MockResponse {
            result: Ok(InvokeResult {
                session_id: "b".into(),
                assistant_text: "build".into(),
                total_cost_usd: 0.01,
                turn_count: 1,
            }),
            sleep: Some(Duration::from_millis(1500)),
        });
        let fsm = Arc::new(CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        ));
        let job_id = "j-pool-cancel".to_string();
        let events = capture_events(&app, &job_id);
        let app_handle = app.handle().clone();
        let fsm_for_task = Arc::clone(&fsm);
        let job_id_for_task = job_id.clone();
        let task = tokio::spawn(async move {
            fsm_for_task
                .run_job_with_id(
                    &app_handle,
                    job_id_for_task,
                    "ws-pool-cancel".into(),
                    "g".into(),
                )
                .await
        });

        // Wait for build's StageStarted then cancel.
        wait_for_events(&events, Duration::from_secs(3), |evts| {
            evts.iter().any(|e| {
                e.kind == KIND_STAGE_STARTED
                    && e.json
                        .get("state")
                        .and_then(|v| v.as_str())
                        == Some("build")
            })
        })
        .await;
        registry.signal_cancel(&job_id).expect("signal");

        let outcome = task.await.expect("task ok").expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert_eq!(
            outcome.last_error.as_deref(),
            Some(CANCELLED_LAST_ERROR)
        );

        let on_disk_state: String = sqlx::query_scalar(
            "SELECT state FROM swarm_jobs WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(on_disk_state, "failed");
        let last_error: Option<String> = sqlx::query_scalar(
            "SELECT last_error FROM swarm_jobs WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(last_error.as_deref(), Some(CANCELLED_LAST_ERROR));
        let lock_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_workspace_locks",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(lock_count, 0);
    }

    // ----------------------------------------------------------------
    // WP-W3-12d — Verdict-driven Review/Test gates
    // ----------------------------------------------------------------

    /// Reviewer returns `approved=false` → job finalizes Failed
    /// with `last_verdict` populated and `last_error == None`. The
    /// Test stage never runs.
    #[tokio::test]
    async fn fsm_review_rejection_finalizes_failed_with_verdict() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), ok_response("s", 0.0));
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        responses.insert(PLANNER_ID.into(), ok_response("p", 0.0));
        responses.insert(BACKEND_BUILDER_ID.into(), ok_response("b", 0.0));
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            rejected_verdict_response("unwrap on None"),
        );
        // Tester response is never consumed (Review rejection on
        // every attempt short-circuits the chain at Review) but
        // the mock requires entries for any id the FSM might ask
        // about; the FSM is careful to NOT call tester on the
        // rejected path.
        responses.insert(TESTER_ID.into(), ok_verdict_response(0.0));

        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-rev-rej".into(), "g".into())
            .await
            .expect("FSM returns Ok with Failed outcome");
        assert_eq!(outcome.final_state, JobState::Failed);
        // W3-12e: Reviewer rejecting on every attempt exhausts the
        // retry budget. Stages: scout (1) + classify (1) + 3 ×
        // (plan/build/review) (= 9) = 11. Test never ran.
        // retry_count = MAX_RETRIES.
        assert_eq!(outcome.stages.len(), 11);
        let final_review = outcome
            .stages
            .iter()
            .rev()
            .find(|s| s.state == JobState::Review)
            .expect("review stage present");
        assert!(final_review.verdict.is_some());
        assert_eq!(
            final_review.verdict.as_ref().map(|v| v.approved),
            Some(false)
        );
        // Job-level last_verdict is the rejected one; last_error
        // stays None (the verdict IS the structured error).
        assert!(outcome.last_error.is_none());
        let lv = outcome.last_verdict.as_ref().expect("last_verdict set");
        assert!(!lv.approved);
        assert_eq!(lv.issues.len(), 1);
        assert_eq!(lv.issues[0].message, "unwrap on None");
    }

    /// Tester returns `approved=false` on every attempt → job
    /// exhausts the retry budget and finalizes Failed at the Test
    /// gate with `last_verdict` populated.
    #[tokio::test]
    async fn fsm_test_rejection_finalizes_failed_with_verdict() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), ok_response("s", 0.0));
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        responses.insert(PLANNER_ID.into(), ok_response("p", 0.0));
        responses.insert(BACKEND_BUILDER_ID.into(), ok_response("b", 0.0));
        responses.insert(BACKEND_REVIEWER_ID.into(), ok_verdict_response(0.0));
        responses.insert(
            TESTER_ID.into(),
            rejected_verdict_response("test_foo failed"),
        );

        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-test-rej".into(), "g".into())
            .await
            .expect("FSM returns Ok with Failed outcome");
        assert_eq!(outcome.final_state, JobState::Failed);
        // W3-12e: Tester rejecting on every attempt exhausts the
        // retry budget. Stages: scout (1) + classify (1) + 3 ×
        // (plan/build/review/test) (= 12) = 14. retry_count =
        // MAX_RETRIES.
        assert_eq!(outcome.stages.len(), 14);
        let final_test = outcome
            .stages
            .iter()
            .rev()
            .find(|s| s.state == JobState::Test)
            .expect("test stage present");
        let test_verdict =
            final_test.verdict.as_ref().expect("test verdict");
        assert!(!test_verdict.approved);
        assert!(outcome.last_error.is_none());
        let lv = outcome.last_verdict.as_ref().expect("last_verdict set");
        assert_eq!(lv.issues[0].message, "test_foo failed");
    }

    /// Reviewer's assistant_text is unparseable (not JSON, not
    /// fenced, not a balanced object). FSM finalizes Failed with
    /// `last_error` mentioning "could not parse Verdict".
    /// `last_verdict` stays None — there's no structured verdict.
    /// W3-12f: classify lands between scout and plan, so the stage
    /// count rises from 4 to 5.
    #[tokio::test]
    async fn fsm_review_unparseable_finalizes_failed() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), ok_response("s", 0.0));
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        responses.insert(PLANNER_ID.into(), ok_response("p", 0.0));
        responses.insert(BACKEND_BUILDER_ID.into(), ok_response("b", 0.0));
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            ok_response("lol idk this isn't JSON", 0.0),
        );
        responses.insert(TESTER_ID.into(), ok_verdict_response(0.0));

        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-rev-bad".into(), "g".into())
            .await
            .expect("FSM returns Ok with Failed outcome");
        assert_eq!(outcome.final_state, JobState::Failed);
        // Review stage IS appended (the run_verdict_stage helper
        // emits StageCompleted on the parse-error path so the W3-14
        // UI sees the raw assistant_text). Stage count = 5
        // (Scout/Classify/Plan/Build/Review).
        assert_eq!(outcome.stages.len(), 5);
        assert_eq!(outcome.stages[4].state, JobState::Review);
        // No structured verdict — parse failed.
        assert!(outcome.stages[4].verdict.is_none());
        assert!(outcome.last_verdict.is_none());
        assert!(
            outcome
                .last_error
                .as_deref()
                .unwrap_or("")
                .contains("could not parse Verdict"),
            "last_error should mention parse failure: {:?}",
            outcome.last_error
        );
    }

    /// Builder errors → no review/test stages run, regression
    /// against the W3-12a failure shape but extended for 5-stage
    /// flow. W3-12f: classify lands between scout and plan, so the
    /// successful-stage count rises from 2 to 3.
    #[tokio::test]
    async fn fsm_review_skipped_when_build_fails() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), ok_response("s", 0.0));
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        responses.insert(PLANNER_ID.into(), ok_response("p", 0.0));
        responses.insert(BACKEND_BUILDER_ID.into(), err_response("builder boom"));
        // These are never consumed since Builder errors first.
        responses.insert(BACKEND_REVIEWER_ID.into(), ok_verdict_response(0.0));
        responses.insert(TESTER_ID.into(), ok_verdict_response(0.0));

        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-skip".into(), "g".into())
            .await
            .expect("FSM returns Ok with Failed outcome");
        assert_eq!(outcome.final_state, JobState::Failed);
        // Scout + Classify + Plan succeeded; Build erred; Review/Test never run.
        assert_eq!(outcome.stages.len(), 3);
        for stage in &outcome.stages {
            assert!(matches!(
                stage.state,
                JobState::Scout | JobState::Classify | JobState::Plan
            ));
            assert!(stage.verdict.is_none());
        }
        assert!(outcome.last_verdict.is_none());
        assert!(outcome
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("builder boom"));
    }

    /// Real-claude integration test (`#[ignore]`) — drives the
    /// full 5-stage chain end-to-end. CI lacks `claude` + an OAuth
    /// session so this stays opt-in. Owner runs it manually with
    /// `cargo test -- --ignored` post-commit.
    ///
    /// Time budget: 5 × 180s = 900s worst-case; typical 2-4 min.
    /// Reviewer should approve a one-line method add;
    /// IntegrationTester runs `cargo check` / `cargo test` to
    /// validate.
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_full_chain_real_claude_with_verdict() {
        use crate::swarm::transport::SubprocessTransport;

        let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(180);

        let (app, _pool, _dir) = mock_app_with_pool().await;
        let profiles = bundled_registry();
        let transport = SubprocessTransport::new();
        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            profiles,
            transport,
            registry,
            Duration::from_secs(stage_secs),
        );
        let goal = "Find the `impl ProfileRegistry` block in \
            profile.rs and add a one-line public method \
            `pub fn profile_count(&self) -> usize { self.profiles.len() }` \
            right after the existing `list` method. Just the method. \
            Do NOT add a unit test, do NOT add doc comments, do NOT \
            run cargo check.";
        let outcome = fsm
            .run_job(app.handle(), "default".into(), goal.into())
            .await
            .expect("FSM returns Ok");
        assert_eq!(
            outcome.final_state,
            JobState::Done,
            "expected Done, got {:?} (last_error: {:?}, last_verdict: {:?})",
            outcome.final_state,
            outcome.last_error,
            outcome.last_verdict,
        );
        // W3-12f: 6 stages on ExecutePlan (scout/classify/plan/build/review/test).
        // The Coordinator brain should pick `execute_plan` for the
        // canonical "add a method" goal.
        assert_eq!(outcome.stages.len(), 6);
    }

    /// Sanity: the registry indeed has the six FSM specialist
    /// ids — guards against future renames in the bundled `.md`
    /// files breaking the FSM contract silently. W3-12f added the
    /// Coordinator brain as the 6th profile.
    #[test]
    fn bundled_registry_has_five_specialist_ids() {
        let reg = bundled_registry();
        for id in [
            SCOUT_ID,
            PLANNER_ID,
            BACKEND_BUILDER_ID,
            BACKEND_REVIEWER_ID,
            TESTER_ID,
            COORDINATOR_ID,
        ] {
            assert!(
                reg.get(id).is_some(),
                "bundled profile `{id}` missing"
            );
        }
        // PathBuf import used only here — silences unused-import
        // warnings if other tests reshape.
        let _ = PathBuf::new();
        let _ = PermissionMode::Plan;
    }

    // ----------------------------------------------------------------
    // WP-W3-12c — streaming events + cancel tests
    // ----------------------------------------------------------------

    use std::sync::Mutex as StdMutex;
    use tauri::Listener;

    /// Discriminator string the FSM emits in the `kind` tag for each
    /// `SwarmJobEvent` variant. Matches the snake_case enum rename;
    /// pulled out so assertions don't repeat magic strings.
    const KIND_STARTED: &str = "started";
    const KIND_STAGE_STARTED: &str = "stage_started";
    const KIND_STAGE_COMPLETED: &str = "stage_completed";
    const KIND_FINISHED: &str = "finished";
    const KIND_CANCELLED: &str = "cancelled";
    const KIND_DECISION_MADE: &str = "decision_made";

    /// One captured event payload from the `swarm:job:{id}:event`
    /// channel. We keep both the parsed JSON value and the raw
    /// payload string so assertions can pick whichever is cleaner.
    #[derive(Debug, Clone)]
    struct CapturedEvent {
        kind: String,
        json: serde_json::Value,
    }

    /// Subscribe to the per-job event channel and stash every event
    /// in an `Arc<Mutex<Vec<...>>>`. Returns the Vec handle so the
    /// test can poll/snapshot it. The listener stays registered
    /// for the test's lifetime; the mock runtime tears it down on
    /// `App` drop.
    fn capture_events<R: tauri::Runtime>(
        app: &tauri::App<R>,
        job_id: &str,
    ) -> Arc<StdMutex<Vec<CapturedEvent>>> {
        let events = Arc::new(StdMutex::new(Vec::<CapturedEvent>::new()));
        let events_w = Arc::clone(&events);
        let event_name = crate::events::swarm_job_event(job_id);
        app.listen(event_name, move |event| {
            let payload = event.payload().to_string();
            // The payload is a JSON object; the `kind` tag rides at
            // the top level per `#[serde(tag = "kind")]`.
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) {
                let kind = value
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                events_w
                    .lock()
                    .expect("captured events lock poisoned")
                    .push(CapturedEvent { kind, json: value });
            }
        });
        events
    }

    /// Wait until the captured events match a predicate, polling on
    /// 10ms ticks. Bounded so a regression surfaces as a test
    /// failure rather than a hang.
    async fn wait_for_events<F>(
        events: &Arc<StdMutex<Vec<CapturedEvent>>>,
        timeout: Duration,
        pred: F,
    ) where
        F: Fn(&[CapturedEvent]) -> bool,
    {
        let start = Instant::now();
        loop {
            {
                let guard = events
                    .lock()
                    .expect("captured events lock poisoned");
                if pred(&guard) {
                    return;
                }
            }
            if start.elapsed() > timeout {
                let snapshot = events
                    .lock()
                    .expect("captured events lock poisoned")
                    .clone();
                panic!(
                    "wait_for_events timed out after {:?}; captured: {:?}",
                    timeout,
                    snapshot
                        .iter()
                        .map(|e| e.kind.clone())
                        .collect::<Vec<_>>()
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Build the canonical scripted-Ok response set with a small
    /// per-stage sleep. The sleep keeps stage durations measurable
    /// for tests that assert on `duration_ms`, but it's incidental
    /// to the streaming-event tests — those rely on
    /// `run_job_with_id` so the listener attaches before any emit.
    ///
    /// W3-12d: the set covers reviewer + tester emit `approved=true`
    /// verdicts. W3-12f: the set also seeds the Coordinator with an
    /// `execute_plan` decision so the chain runs end to end.
    fn happy_responses() -> HashMap<String, MockResponse> {
        let mut r: HashMap<String, MockResponse> = HashMap::new();
        let sleep = Some(Duration::from_millis(100));
        let stages: &[(&str, &str, &str, f64)] = &[
            (SCOUT_ID, "sess-scout", "scout findings", 0.01),
            (PLANNER_ID, "sess-plan", "plan steps", 0.02),
            (BACKEND_BUILDER_ID, "sess-build", "build done", 0.03),
        ];
        for (id, sess, text, cost) in stages {
            r.insert(
                (*id).to_string(),
                MockResponse {
                    result: Ok(InvokeResult {
                        session_id: (*sess).into(),
                        assistant_text: (*text).into(),
                        total_cost_usd: *cost,
                        turn_count: 1,
                    }),
                    sleep,
                },
            );
        }
        let verdict_text =
            r#"{"approved":true,"issues":[],"summary":"OK"}"#;
        for (id, sess) in [
            (BACKEND_REVIEWER_ID, "sess-review"),
            (TESTER_ID, "sess-test"),
        ] {
            r.insert(
                id.to_string(),
                MockResponse {
                    result: Ok(InvokeResult {
                        session_id: sess.into(),
                        assistant_text: verdict_text.into(),
                        total_cost_usd: 0.0,
                        turn_count: 1,
                    }),
                    sleep,
                },
            );
        }
        // Coordinator brain (W3-12f) — votes `execute_plan` so the
        // existing 5-stage chain runs after Classify lands.
        r.insert(
            COORDINATOR_ID.to_string(),
            MockResponse {
                result: Ok(InvokeResult {
                    session_id: "sess-coord".into(),
                    assistant_text:
                        r#"{"route":"execute_plan","scope":"backend","reasoning":"mock"}"#
                            .into(),
                    total_cost_usd: 0.0,
                    turn_count: 1,
                }),
                sleep,
            },
        );
        r
    }

    /// Happy path: subscribe before run, observe the full ordered
    /// stream `Started → StageStarted → StageCompleted` × 3 →
    /// `Finished`. Uses `run_job_with_id` so the listener can
    /// attach before any event fires.
    #[tokio::test]
    async fn events_started_fires_first_then_stage_pairs_then_finished() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let mock = MockTransport::new(happy_responses());
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            mock,
            Arc::clone(&registry),
            Duration::from_secs(5),
        );

        let job_id = "j-test-happy".to_string();
        let events = capture_events(&app, &job_id);

        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-events-happy".into(),
                "do thing".into(),
            )
            .await
            .expect("happy path returns Ok");
        assert_eq!(outcome.final_state, JobState::Done);

        wait_for_events(&events, Duration::from_secs(2), |evts| {
            evts.iter().any(|e| e.kind == KIND_FINISHED)
        })
        .await;

        let captured = events
            .lock()
            .expect("captured events lock poisoned")
            .clone();
        let kinds: Vec<&str> =
            captured.iter().map(|e| e.kind.as_str()).collect();
        // Exact ordered stream on the W3-12f ExecutePlan happy path:
        // started → 6 × (stage_started, stage_completed) with
        // DecisionMade interleaved between Classify's
        // stage_completed and Plan's stage_started → finished.
        assert_eq!(
            kinds,
            vec![
                KIND_STARTED,
                // Scout
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                // Classify
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                KIND_DECISION_MADE,
                // Plan
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                // Build
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                // Review
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                // Test
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                KIND_FINISHED,
            ],
            "full ordered stream mismatch; kinds={kinds:?}"
        );

        // Each StageStarted carries the upcoming state; assert the
        // canonical `scout → classify → plan → build → review → test` order.
        let stage_starts: Vec<&str> = captured
            .iter()
            .filter(|e| e.kind == KIND_STAGE_STARTED)
            .map(|e| {
                e.json
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            })
            .collect();
        assert_eq!(
            stage_starts,
            vec!["scout", "classify", "plan", "build", "review", "test"]
        );
    }

    /// `Finished.outcome` deeply equals the IPC return value's key
    /// fields. We don't compare full structs (the listener loses
    /// strict typing through JSON), but we assert `final_state`,
    /// `stages.len()`, and `total_cost_usd`.
    #[tokio::test]
    async fn events_finished_carries_full_outcome() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let mock = MockTransport::new(happy_responses());
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            mock,
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-test-outcome".to_string();
        let events = capture_events(&app, &job_id);
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-events-outcome".into(),
                "g".into(),
            )
            .await
            .expect("happy path returns Ok");

        wait_for_events(&events, Duration::from_secs(2), |evts| {
            evts.iter().any(|e| e.kind == KIND_FINISHED)
        })
        .await;
        let captured = events
            .lock()
            .expect("captured events lock poisoned")
            .clone();
        let finished = captured
            .iter()
            .find(|e| e.kind == KIND_FINISHED)
            .expect("Finished event captured");
        let outcome_json = finished
            .json
            .get("outcome")
            .expect("Finished carries outcome");
        // final_state is camelCased per JobOutcome's serde renames.
        let final_state = outcome_json
            .get("finalState")
            .and_then(|v| v.as_str())
            .expect("finalState string");
        assert_eq!(final_state, "done");
        let stages = outcome_json
            .get("stages")
            .and_then(|v| v.as_array())
            .expect("stages array");
        assert_eq!(stages.len(), outcome.stages.len());
        assert_eq!(stages.len(), 6);
        let total_cost = outcome_json
            .get("totalCostUsd")
            .and_then(|v| v.as_f64())
            .expect("totalCostUsd number");
        assert!((total_cost - outcome.total_cost_usd).abs() < 1e-9);
    }

    /// Stage-error path: planner fails. Events for the failing
    /// stage include `StageStarted(Plan)` but NO
    /// `StageCompleted(Plan)`, then `Finished`. No Build events
    /// either.
    #[tokio::test]
    async fn events_stage_failure_skips_subsequent_stages() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), MockResponse {
            result: Ok(InvokeResult {
                session_id: "s".into(),
                assistant_text: "scout".into(),
                total_cost_usd: 0.01,
                turn_count: 1,
            }),
            sleep: Some(Duration::from_millis(100)),
        });
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        responses.insert(PLANNER_ID.into(), MockResponse {
            result: Err(AppError::SwarmInvoke("planner boom".into())),
            sleep: Some(Duration::from_millis(100)),
        });
        responses.insert(BACKEND_BUILDER_ID.into(), ok_response("unused", 0.0));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-test-fail".to_string();
        let events = capture_events(&app, &job_id);
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-events-fail".into(),
                "g".into(),
            )
            .await
            .expect("FSM returns Ok");
        assert_eq!(outcome.final_state, JobState::Failed);

        wait_for_events(&events, Duration::from_secs(2), |evts| {
            evts.iter().any(|e| e.kind == KIND_FINISHED)
        })
        .await;
        let captured = events
            .lock()
            .expect("captured events lock poisoned")
            .clone();
        let kinds: Vec<&str> =
            captured.iter().map(|e| e.kind.as_str()).collect();
        // Last event is Finished.
        assert_eq!(kinds.last().copied(), Some(KIND_FINISHED));
        // No stage_completed for the planner stage's state, nor any
        // build-stage events.
        let plan_starts: Vec<&CapturedEvent> = captured
            .iter()
            .filter(|e| {
                e.kind == KIND_STAGE_STARTED
                    && e.json
                        .get("state")
                        .and_then(|v| v.as_str())
                        == Some("plan")
            })
            .collect();
        assert_eq!(plan_starts.len(), 1, "exactly one StageStarted(Plan)");
        let plan_completes: Vec<&CapturedEvent> = captured
            .iter()
            .filter(|e| {
                e.kind == KIND_STAGE_COMPLETED
                    && e.json
                        .get("stage")
                        .and_then(|s| s.get("state"))
                        .and_then(|v| v.as_str())
                        == Some("plan")
            })
            .collect();
        assert_eq!(
            plan_completes.len(),
            0,
            "no StageCompleted(Plan) on the failure path"
        );
        let build_events: Vec<&CapturedEvent> = captured
            .iter()
            .filter(|e| {
                let s = e.json.get("state").and_then(|v| v.as_str());
                let stage_state = e
                    .json
                    .get("stage")
                    .and_then(|st| st.get("state"))
                    .and_then(|v| v.as_str());
                s == Some("build") || stage_state == Some("build")
            })
            .collect();
        assert!(
            build_events.is_empty(),
            "no Build events on the planner-failure path; got: {:?}",
            build_events
        );
    }

    /// Cancel during the SCOUT stage. Use a slow mock so the cancel
    /// signal can land mid-stage.
    #[tokio::test]
    async fn cancel_during_scout_fires_cancelled_then_finished() {
        cancel_during_stage_helper(
            JobState::Scout,
            "scout",
            true,  // delay scout enough that cancel always lands in scout
        )
        .await;
    }

    #[tokio::test]
    async fn cancel_during_plan_fires_cancelled_with_plan_state() {
        cancel_during_stage_helper(JobState::Plan, "plan", false).await;
    }

    #[tokio::test]
    async fn cancel_during_build_fires_cancelled_with_build_state() {
        cancel_during_stage_helper(JobState::Build, "build", false).await;
    }

    /// Drives a cancel against `target_state` by:
    /// 1. Starting the FSM with stages before `target_state` fast
    ///    (10ms sleep) and `target_state` itself slow (1500ms).
    /// 2. Discovering the job id, subscribing to events.
    /// 3. Waiting for `StageStarted` matching the target state.
    /// 4. Calling `signal_cancel(&job_id)`.
    /// 5. Asserting `Cancelled { cancelled_during: target }` and
    ///    `Finished` arrive, and `last_error` is the canonical
    ///    cancel string.
    async fn cancel_during_stage_helper(
        target_state: JobState,
        target_state_wire: &str,
        force_long_first_stage: bool,
    ) {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());

        let fast = Duration::from_millis(10);
        let slow = Duration::from_millis(1500);
        let scout_sleep = if matches!(target_state, JobState::Scout)
            || force_long_first_stage
        {
            Some(slow)
        } else {
            Some(fast)
        };
        let plan_sleep = if matches!(target_state, JobState::Plan) {
            Some(slow)
        } else {
            Some(fast)
        };
        let build_sleep = if matches!(target_state, JobState::Build) {
            Some(slow)
        } else {
            Some(fast)
        };

        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), MockResponse {
            result: Ok(InvokeResult {
                session_id: "s".into(),
                assistant_text: "scout".into(),
                total_cost_usd: 0.01,
                turn_count: 1,
            }),
            sleep: scout_sleep,
        });
        // Coordinator brain (W3-12f) — fast ExecutePlan decision so
        // the cancel target downstream of Classify is reached.
        responses.insert(COORDINATOR_ID.into(), execute_plan_decision_response());
        responses.insert(PLANNER_ID.into(), MockResponse {
            result: Ok(InvokeResult {
                session_id: "p".into(),
                assistant_text: "plan".into(),
                total_cost_usd: 0.01,
                turn_count: 1,
            }),
            sleep: plan_sleep,
        });
        responses.insert(BACKEND_BUILDER_ID.into(), MockResponse {
            result: Ok(InvokeResult {
                session_id: "b".into(),
                assistant_text: "build".into(),
                total_cost_usd: 0.01,
                turn_count: 1,
            }),
            sleep: build_sleep,
        });
        let fsm = Arc::new(CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        ));

        let job_id = format!("j-test-cancel-{target_state_wire}");
        let events = capture_events(&app, &job_id);

        // Spawn the FSM and let the listener catch every event from
        // the very first emit.
        let app_handle = app.handle().clone();
        let fsm_for_task = Arc::clone(&fsm);
        let job_id_for_task = job_id.clone();
        let workspace_id = format!("ws-cancel-{target_state_wire}");
        let handle = tokio::spawn(async move {
            fsm_for_task
                .run_job_with_id(
                    &app_handle,
                    job_id_for_task,
                    workspace_id,
                    "g".into(),
                )
                .await
        });

        // Wait for StageStarted matching target_state.
        wait_for_events(&events, Duration::from_secs(3), |evts| {
            evts.iter().any(|e| {
                e.kind == KIND_STAGE_STARTED
                    && e.json
                        .get("state")
                        .and_then(|v| v.as_str())
                        == Some(target_state_wire)
            })
        })
        .await;

        // Signal cancel.
        registry
            .signal_cancel(&job_id)
            .expect("signal cancel ok");

        // Wait for Cancelled then Finished.
        wait_for_events(&events, Duration::from_secs(3), |evts| {
            evts.iter().any(|e| e.kind == KIND_CANCELLED)
                && evts.iter().any(|e| e.kind == KIND_FINISHED)
        })
        .await;

        let outcome =
            handle.await.expect("task ok").expect("FSM returns Ok");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert_eq!(
            outcome.last_error.as_deref(),
            Some(CANCELLED_LAST_ERROR)
        );

        let captured = events
            .lock()
            .expect("captured events lock poisoned")
            .clone();
        let cancelled = captured
            .iter()
            .find(|e| e.kind == KIND_CANCELLED)
            .expect("Cancelled event captured");
        let cancelled_during = cancelled
            .json
            .get("cancelled_during")
            .and_then(|v| v.as_str())
            .expect("cancelled_during str");
        assert_eq!(cancelled_during, target_state_wire);

        // Last event must be Finished.
        let kinds: Vec<&str> =
            captured.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds.last().copied(),
            Some(KIND_FINISHED),
            "last event must be Finished; full sequence: {:?}",
            kinds
        );
    }

    // ----------------------------------------------------------------
    // WP-W3-12e — retry feedback loop tests
    // ----------------------------------------------------------------

    /// Sequenced mock transport: pops one response per `profile.id`
    /// per call, so retry-driven tests can script Reviewer to return
    /// rejected-then-approved across two attempts. Records every
    /// prompt the FSM sent for post-run assertion.
    ///
    /// Build via [`SequencedMock::new`] passing a HashMap from
    /// profile id to a `Vec<MockResponse>`. The first call for a
    /// given id consumes index 0, the second consumes index 1, etc.
    /// If the queue is exhausted the transport returns the last
    /// response repeatedly so a test that scripts e.g. Scout once
    /// doesn't have to spell out a placeholder for every retry.
    struct SequencedMock {
        responses: std::sync::Mutex<HashMap<String, Vec<MockResponse>>>,
        cursor: std::sync::Mutex<HashMap<String, usize>>,
        seen: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl SequencedMock {
        fn new(responses: HashMap<String, Vec<MockResponse>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
                cursor: std::sync::Mutex::new(HashMap::new()),
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<(String, String)> {
            self.seen.lock().expect("seen poisoned").clone()
        }

        fn call_count(&self, id: &str) -> usize {
            self.seen
                .lock()
                .expect("seen poisoned")
                .iter()
                .filter(|(pid, _)| pid == id)
                .count()
        }
    }

    impl Transport for SequencedMock {
        async fn invoke<R: Runtime>(
            &self,
            _app: &AppHandle<R>,
            profile: &Profile,
            user_message: &str,
            _timeout: Duration,
        ) -> Result<InvokeResult, AppError> {
            self.seen
                .lock()
                .expect("seen poisoned")
                .push((profile.id.clone(), user_message.to_string()));

            // Pull the response for this call, then advance the cursor.
            let response_clone = {
                let responses_guard =
                    self.responses.lock().expect("responses poisoned");
                let queue = responses_guard.get(&profile.id).ok_or_else(|| {
                    AppError::SwarmInvoke(format!(
                        "SequencedMock: no scripted responses for `{}`",
                        profile.id
                    ))
                })?;
                if queue.is_empty() {
                    return Err(AppError::SwarmInvoke(format!(
                        "SequencedMock: empty queue for `{}`",
                        profile.id
                    )));
                }
                let mut cursor_guard =
                    self.cursor.lock().expect("cursor poisoned");
                let idx = cursor_guard
                    .entry(profile.id.clone())
                    .or_insert(0);
                let pick = (*idx).min(queue.len() - 1);
                if *idx < queue.len() - 1 {
                    *idx += 1;
                }
                let entry = &queue[pick];
                MockResponse {
                    result: entry.result.clone(),
                    sleep: entry.sleep,
                }
            };

            if let Some(sleep) = response_clone.sleep {
                tokio::time::sleep(sleep).await;
            }
            response_clone.result
        }
    }

    /// Wrapper letting an `&Arc<SequencedMock>` satisfy `T: Transport`
    /// so prompt-inspection tests can keep a handle on the mock for
    /// post-run `seen()` while still passing it by value to
    /// `CoordinatorFsm::new`.
    struct ArcSequencedMock(Arc<SequencedMock>);

    impl Transport for ArcSequencedMock {
        async fn invoke<R: Runtime>(
            &self,
            app: &AppHandle<R>,
            profile: &Profile,
            user_message: &str,
            timeout: Duration,
        ) -> Result<InvokeResult, AppError> {
            self.0.invoke(app, profile, user_message, timeout).await
        }
    }

    /// Build a rejected-verdict response with a custom severity +
    /// file/line so the retry-prompt tests can assert on bullet
    /// content. Differs from `rejected_verdict_response` (which only
    /// supports the high-severity / no-file shape).
    fn rich_rejected_verdict(
        severity: &str,
        file: &str,
        line: u32,
        msg: &str,
    ) -> MockResponse {
        let payload = format!(
            r#"{{"approved":false,"issues":[{{"severity":"{sev}","file":"{file}","line":{line},"msg":"{msg}"}}],"summary":"rejected"}}"#,
            sev = severity,
            file = file,
            line = line,
            msg = msg.replace('\\', "\\\\").replace('"', "\\\""),
        );
        MockResponse {
            result: Ok(InvokeResult {
                session_id: "sess-rich-rej".into(),
                assistant_text: payload,
                total_cost_usd: 0.0,
                turn_count: 1,
            }),
            sleep: None,
        }
    }

    /// Reviewer rejects on attempt 1, approves on attempt 2.
    /// Tester always approves. Final state Done, retry_count=1.
    /// W3-12f: stages: scout (1) + classify (1) + 2 × (plan/build/review)
    /// (= 6) + test (1) = 9. RetryStarted event fires once; Scout
    /// and Classify each run once.
    #[tokio::test]
    async fn fsm_review_reject_retries_within_budget() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());

        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(
            SCOUT_ID.into(),
            vec![ok_response("scout findings", 0.0)],
        );
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_decision_response()],
        );
        responses.insert(
            PLANNER_ID.into(),
            vec![ok_response("plan-A", 0.0), ok_response("plan-B", 0.0)],
        );
        responses.insert(
            BACKEND_BUILDER_ID.into(),
            vec![ok_response("build-A", 0.0), ok_response("build-B", 0.0)],
        );
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![
                rejected_verdict_response("missing import"),
                ok_verdict_response(0.0),
            ],
        );
        responses.insert(TESTER_ID.into(), vec![ok_verdict_response(0.0)]);

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );

        let job_id = "j-rev-retry-ok".to_string();
        let events = capture_events(&app, &job_id);
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-rev-retry".into(),
                "g".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 9, "1 scout + 1 classify + 2x(plan/build/review) + test");
        // Scout + Classify each invoked exactly once across retries.
        assert_eq!(mock.call_count(SCOUT_ID), 1);
        assert_eq!(mock.call_count(COORDINATOR_ID), 1);
        // Job-level retry_count == 1 (one full retry round).
        let job = registry.get(&job_id).expect("job in registry");
        assert_eq!(job.retry_count, 1);

        // RetryStarted fires once.
        let captured = events
            .lock()
            .expect("events poisoned")
            .clone();
        let retries: Vec<&CapturedEvent> = captured
            .iter()
            .filter(|e| e.kind == "retry_started")
            .collect();
        assert_eq!(retries.len(), 1, "one retry round");
        let retry = retries[0];
        assert_eq!(
            retry.json.get("attempt").and_then(|v| v.as_u64()),
            Some(2),
            "first retry is attempt=2"
        );
        assert_eq!(
            retry.json.get("triggered_by").and_then(|v| v.as_str()),
            Some("review")
        );
    }

    /// Reviewer rejects on every attempt → retry budget exhausts at
    /// 3 attempts. Final Failed, retry_count=2.
    /// W3-12f: stages = 1 (scout) + 1 (classify) + 3×3 (plan/build/review) = 11.
    /// RetryStarted fires twice.
    #[tokio::test]
    async fn fsm_review_reject_exhausts_retries() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());

        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_decision_response()],
        );
        responses.insert(PLANNER_ID.into(), vec![ok_response("p", 0.0)]);
        responses.insert(BACKEND_BUILDER_ID.into(), vec![ok_response("b", 0.0)]);
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![rejected_verdict_response("rev fail")],
        );
        responses.insert(TESTER_ID.into(), vec![ok_verdict_response(0.0)]);

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-rev-exhaust".to_string();
        let events = capture_events(&app, &job_id);
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-rev-exhaust".into(),
                "g".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert_eq!(outcome.stages.len(), 11);
        let job = registry.get(&job_id).expect("job");
        assert_eq!(job.retry_count, MAX_RETRIES);
        assert!(outcome.last_verdict.is_some());

        // RetryStarted fires twice (between attempts 1→2 and 2→3).
        let captured = events.lock().expect("events poisoned").clone();
        let retries: Vec<&CapturedEvent> = captured
            .iter()
            .filter(|e| e.kind == "retry_started")
            .collect();
        assert_eq!(retries.len(), 2);
        // First retry: attempt=2; second retry: attempt=3.
        assert_eq!(
            retries[0].json.get("attempt").and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            retries[1].json.get("attempt").and_then(|v| v.as_u64()),
            Some(3)
        );
        // Tester never runs (Reviewer always rejects first).
        assert_eq!(mock.call_count(TESTER_ID), 0);
    }

    /// Reviewer always approves; Tester rejects then approves.
    /// Final Done, retry_count=1.
    #[tokio::test]
    async fn fsm_test_reject_retries_then_passes() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());

        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_decision_response()],
        );
        responses.insert(
            PLANNER_ID.into(),
            vec![ok_response("p1", 0.0), ok_response("p2", 0.0)],
        );
        responses.insert(
            BACKEND_BUILDER_ID.into(),
            vec![ok_response("b1", 0.0), ok_response("b2", 0.0)],
        );
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![ok_verdict_response(0.0)],
        );
        responses.insert(
            TESTER_ID.into(),
            vec![
                rejected_verdict_response("flaky test"),
                ok_verdict_response(0.0),
            ],
        );

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-test-retry-ok".to_string();
        let events = capture_events(&app, &job_id);
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-test-retry".into(),
                "g".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        // W3-12f: 1 scout + 1 classify + 2x(plan/build/review/test) = 10.
        assert_eq!(outcome.stages.len(), 10);
        let job = registry.get(&job_id).expect("job");
        assert_eq!(job.retry_count, 1);

        let captured = events.lock().expect("events poisoned").clone();
        let retry = captured
            .iter()
            .find(|e| e.kind == "retry_started")
            .expect("RetryStarted fired");
        assert_eq!(
            retry.json.get("triggered_by").and_then(|v| v.as_str()),
            Some("test")
        );
    }

    /// Tester rejects on every attempt → retry budget exhausts.
    /// Final Failed, retry_count=2.
    /// W3-12f: stages = 1 (scout) + 1 (classify) + 3×4 (plan/build/review/test) = 14.
    #[tokio::test]
    async fn fsm_test_reject_exhausts_retries() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());

        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_decision_response()],
        );
        responses.insert(PLANNER_ID.into(), vec![ok_response("p", 0.0)]);
        responses.insert(BACKEND_BUILDER_ID.into(), vec![ok_response("b", 0.0)]);
        responses.insert(BACKEND_REVIEWER_ID.into(), vec![ok_verdict_response(0.0)]);
        responses.insert(
            TESTER_ID.into(),
            vec![rejected_verdict_response("test fail")],
        );

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-test-exhaust".to_string();
        let events = capture_events(&app, &job_id);
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-test-exhaust".into(),
                "g".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert_eq!(outcome.stages.len(), 14);
        let job = registry.get(&job_id).expect("job");
        assert_eq!(job.retry_count, MAX_RETRIES);

        let captured = events.lock().expect("events poisoned").clone();
        let retries: Vec<&CapturedEvent> = captured
            .iter()
            .filter(|e| e.kind == "retry_started")
            .collect();
        assert_eq!(retries.len(), 2);
        for r in &retries {
            assert_eq!(
                r.json.get("triggered_by").and_then(|v| v.as_str()),
                Some("test")
            );
        }
    }

    /// Scout runs exactly once across the full retry chain. Verified
    /// against the SequencedMock's call counter.
    #[tokio::test]
    async fn fsm_scout_runs_once_across_retries() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());

        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        // Scout queue has just one entry. Two retries → if Scout ran
        // 3 times the SequencedMock would clamp to the last entry,
        // but we assert the call counter directly so the regression
        // is unambiguous.
        responses.insert(SCOUT_ID.into(), vec![ok_response("scout", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_decision_response()],
        );
        responses.insert(PLANNER_ID.into(), vec![ok_response("p", 0.0)]);
        responses.insert(BACKEND_BUILDER_ID.into(), vec![ok_response("b", 0.0)]);
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![rejected_verdict_response("force retry")],
        );
        responses.insert(TESTER_ID.into(), vec![ok_verdict_response(0.0)]);

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        fsm.run_job_with_id(
            app.handle(),
            "j-scout-once".into(),
            "ws-scout-once".into(),
            "g".into(),
        )
        .await
        .expect("FSM ok");
        assert_eq!(
            mock.call_count(SCOUT_ID),
            1,
            "Scout must run exactly once even across retries"
        );
        // W3-12f: Coordinator brain also runs exactly once per job
        // (Classify is a one-shot decision, not part of the retry
        // loop).
        assert_eq!(
            mock.call_count(COORDINATOR_ID),
            1,
            "Coordinator must run exactly once even across retries"
        );
        // Sanity: planner ran 3 times (one per attempt).
        assert_eq!(mock.call_count(PLANNER_ID), 3);
    }

    /// On retry, the second Plan call's prompt contains the rejecting
    /// gate label, the verdict's issue bullets (with severity and
    /// file:line), and the previous plan text.
    #[tokio::test]
    async fn retry_plan_prompt_includes_verdict_issues() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());

        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("scout-out", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_decision_response()],
        );
        responses.insert(
            PLANNER_ID.into(),
            vec![
                ok_response("PREVIOUS-PLAN-TEXT", 0.0),
                ok_response("p2", 0.0),
            ],
        );
        responses.insert(
            BACKEND_BUILDER_ID.into(),
            vec![ok_response("b1", 0.0), ok_response("b2", 0.0)],
        );
        // Rejecting verdict carries one high-severity issue with
        // file+line populated so the bullet renders the [high] tag
        // and the "src/foo.rs:42" location.
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![
                rich_rejected_verdict("high", "src/foo.rs", 42, "missing semicolon"),
                ok_verdict_response(0.0),
            ],
        );
        responses.insert(TESTER_ID.into(), vec![ok_verdict_response(0.0)]);

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-prompt".into(), "G".into())
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);

        let seen = mock.seen();
        let plan_prompts: Vec<&str> = seen
            .iter()
            .filter(|(id, _)| id == PLANNER_ID)
            .map(|(_, p)| p.as_str())
            .collect();
        assert_eq!(plan_prompts.len(), 2);
        let retry_prompt = plan_prompts[1];
        // Issue bullet shape: severity tag + file:line + message.
        assert!(
            retry_prompt.contains("[high]"),
            "retry prompt missing [high] tag: {retry_prompt}"
        );
        assert!(
            retry_prompt.contains("src/foo.rs:42"),
            "retry prompt missing file:line: {retry_prompt}"
        );
        assert!(
            retry_prompt.contains("missing semicolon"),
            "retry prompt missing issue message: {retry_prompt}"
        );
        // Gate name renders as "Reviewer" (Review-rejected gate).
        assert!(
            retry_prompt.contains("Reviewer"),
            "retry prompt missing gate label: {retry_prompt}"
        );
        // Previous plan body is quoted verbatim.
        assert!(
            retry_prompt.contains("PREVIOUS-PLAN-TEXT"),
            "retry prompt missing previous plan: {retry_prompt}"
        );
    }

    /// First-attempt Plan prompt is the canonical (non-retry)
    /// template — no "REDDEDİLDİ" header, no issue bullets.
    #[tokio::test]
    async fn retry_plan_prompt_omitted_on_first_attempt() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let responses = happy_5_stage_responses(
            "scout", "plan", "build", 0.0, 0.0, 0.0,
        );
        let mock = Arc::new(MockTransport::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcTransport(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        fsm.run_job(app.handle(), "ws-first-attempt".into(), "G".into())
            .await
            .expect("FSM ok");
        let seen = mock.seen();
        let plan_prompt = seen
            .iter()
            .find(|(id, _)| id == PLANNER_ID)
            .map(|(_, p)| p.as_str())
            .expect("plan prompt recorded");
        assert!(
            !plan_prompt.contains("REDDEDİLDİ"),
            "first-attempt prompt should not be the retry variant: {plan_prompt}"
        );
        assert!(
            !plan_prompt.contains("Önceki plan"),
            "first-attempt prompt should not quote a previous plan: {plan_prompt}"
        );
    }

    /// `RetryStarted.attempt` is 1-indexed: 2 on the first retry, 3
    /// on the second.
    #[tokio::test]
    async fn retry_started_event_fires_with_correct_attempt_number() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_decision_response()],
        );
        responses.insert(PLANNER_ID.into(), vec![ok_response("p", 0.0)]);
        responses.insert(BACKEND_BUILDER_ID.into(), vec![ok_response("b", 0.0)]);
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![rejected_verdict_response("force retries")],
        );
        responses.insert(TESTER_ID.into(), vec![ok_verdict_response(0.0)]);
        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-attempt-numbers".to_string();
        let events = capture_events(&app, &job_id);
        fsm.run_job_with_id(
            app.handle(),
            job_id.clone(),
            "ws-attempt".into(),
            "G".into(),
        )
        .await
        .expect("FSM ok");
        let captured = events.lock().expect("events poisoned").clone();
        let attempts: Vec<u64> = captured
            .iter()
            .filter(|e| e.kind == "retry_started")
            .filter_map(|e| e.json.get("attempt").and_then(|v| v.as_u64()))
            .collect();
        assert_eq!(attempts, vec![2, 3]);
    }

    /// `retry_count` round-trips through SQLite via the registry's
    /// write-through. Mock-driven: write retry_count=1 on a
    /// pool-backed registry, reload via `get_job_detail`, assert.
    #[tokio::test]
    async fn retry_count_persists_across_app_restart() {
        let (registry, _pool, _reg_dir) = pool_backed_registry().await;
        // Acquire a fresh job slot so `update` can mutate it.
        let job = Job {
            id: "j-restart".into(),
            goal: "g".into(),
            created_at_ms: 0,
            state: JobState::Init,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
            last_verdict: None,
        };
        registry
            .try_acquire_workspace("ws-restart", job)
            .await
            .expect("acquire");
        registry
            .update("j-restart", |j| {
                j.retry_count = 1;
            })
            .await
            .expect("update retry_count");

        // Reload via the SQL read path — same surface the IPC layer
        // uses post-restart.
        let detail = registry
            .get_job_detail("j-restart")
            .await
            .expect("query")
            .expect("row exists");
        assert_eq!(detail.retry_count, 1);
    }

    /// Cancel during the retry attempt's Build stage. Drives
    /// attempt 1 to a Reviewer rejection, then on attempt 2 the
    /// Build mock is slow enough that signal_cancel lands
    /// mid-Build. `Cancelled.cancelled_during == "build"`.
    #[tokio::test]
    async fn cancel_during_retry_attempt_2_records_correct_stage() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());

        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_decision_response()],
        );
        // Plan: fast on both attempts.
        responses.insert(
            PLANNER_ID.into(),
            vec![ok_response("p1", 0.0), ok_response("p2", 0.0)],
        );
        // Build: fast on attempt 1, slow on attempt 2 so cancel lands
        // mid-Build.
        responses.insert(
            BACKEND_BUILDER_ID.into(),
            vec![
                ok_response("b1", 0.0),
                MockResponse {
                    result: Ok(InvokeResult {
                        session_id: "b2".into(),
                        assistant_text: "b2".into(),
                        total_cost_usd: 0.0,
                        turn_count: 1,
                    }),
                    sleep: Some(Duration::from_millis(1500)),
                },
            ],
        );
        // Reviewer rejects attempt 1, never reached on attempt 2.
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![rejected_verdict_response("fix me")],
        );
        responses.insert(TESTER_ID.into(), vec![ok_verdict_response(0.0)]);

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = Arc::new(CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        ));

        let job_id = "j-cancel-retry".to_string();
        let events = capture_events(&app, &job_id);
        let app_handle = app.handle().clone();
        let fsm_for_task = Arc::clone(&fsm);
        let job_id_for_task = job_id.clone();
        let task = tokio::spawn(async move {
            fsm_for_task
                .run_job_with_id(
                    &app_handle,
                    job_id_for_task,
                    "ws-cancel-retry".into(),
                    "G".into(),
                )
                .await
        });

        // Wait for RetryStarted then for the Build StageStarted of
        // attempt 2 — at that point attempt-2's Build is awaiting
        // the slow mock and a cancel signal will be caught by the
        // tokio::select! in `run_stage_with_cancel`.
        wait_for_events(&events, Duration::from_secs(3), |evts| {
            evts.iter().any(|e| e.kind == "retry_started")
        })
        .await;
        // The retry transitions back to Plan; once we've seen a
        // StageStarted(build) AFTER the retry_started, we know we're
        // mid-Build of attempt 2.
        wait_for_events(&events, Duration::from_secs(3), |evts| {
            // last build StageStarted index must come AFTER the
            // retry_started index.
            let retry_idx = evts
                .iter()
                .position(|e| e.kind == "retry_started");
            let build_idx = evts
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e.kind == KIND_STAGE_STARTED
                        && e.json.get("state").and_then(|v| v.as_str())
                            == Some("build")
                })
                .map(|(i, _)| i)
                .next_back();
            matches!((retry_idx, build_idx), (Some(r), Some(b)) if b > r)
        })
        .await;

        registry.signal_cancel(&job_id).expect("signal cancel ok");
        let outcome =
            task.await.expect("task ok").expect("FSM returns Ok");
        assert_eq!(outcome.final_state, JobState::Failed);
        assert_eq!(
            outcome.last_error.as_deref(),
            Some(CANCELLED_LAST_ERROR)
        );

        let captured = events.lock().expect("events poisoned").clone();
        let cancelled = captured
            .iter()
            .find(|e| e.kind == KIND_CANCELLED)
            .expect("Cancelled event captured");
        assert_eq!(
            cancelled
                .json
                .get("cancelled_during")
                .and_then(|v| v.as_str()),
            Some("build")
        );
    }

    /// Mixed rejections: Reviewer rejects on attempt 1; on attempt 2
    /// Reviewer approves but Tester rejects; on attempt 3 both
    /// approve. Final Done, retry_count=2.
    #[tokio::test]
    async fn mixed_review_then_test_rejection_uses_one_retry_each() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_decision_response()],
        );
        responses.insert(
            PLANNER_ID.into(),
            vec![
                ok_response("p1", 0.0),
                ok_response("p2", 0.0),
                ok_response("p3", 0.0),
            ],
        );
        responses.insert(
            BACKEND_BUILDER_ID.into(),
            vec![
                ok_response("b1", 0.0),
                ok_response("b2", 0.0),
                ok_response("b3", 0.0),
            ],
        );
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![
                rejected_verdict_response("rev fail"),
                ok_verdict_response(0.0),
                ok_verdict_response(0.0),
            ],
        );
        responses.insert(
            TESTER_ID.into(),
            vec![
                rejected_verdict_response("test fail"),
                ok_verdict_response(0.0),
            ],
        );
        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-mixed".to_string();
        let events = capture_events(&app, &job_id);
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-mixed".into(),
                "G".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        let job = registry.get(&job_id).expect("job");
        assert_eq!(job.retry_count, 2);
        // W3-12f: 1 scout + 1 classify + attempt 1 (plan/build/review)
        // + attempt 2 (plan/build/review/test) + attempt 3
        // (plan/build/review/test) = 1 + 1 + 3 + 4 + 4 = 13.
        assert_eq!(outcome.stages.len(), 13);

        let captured = events.lock().expect("events poisoned").clone();
        let triggers: Vec<&str> = captured
            .iter()
            .filter(|e| e.kind == "retry_started")
            .filter_map(|e| {
                e.json.get("triggered_by").and_then(|v| v.as_str())
            })
            .collect();
        assert_eq!(triggers, vec!["review", "test"]);
    }

    // ----------------------------------------------------------------
    // WP-W3-12f — Coordinator brain (Classify) tests
    // ----------------------------------------------------------------

    /// Mock-driven research-only path: Coordinator brain returns
    /// `route: research_only`; FSM finalizes Done after Classify
    /// without invoking Plan/Build/Review/Test. Stage count = 2
    /// (Scout + Classify).
    #[tokio::test]
    async fn fsm_classify_research_only_skips_to_done() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), ok_response("scout findings", 0.0));
        responses.insert(
            COORDINATOR_ID.into(),
            research_only_decision_response(),
        );
        // The downstream specialists must NOT be invoked on this
        // path; we still seed entries so a regression that calls
        // them would surface as unrelated assertion failures rather
        // than a "no scripted response" mock error.
        responses.insert(PLANNER_ID.into(), ok_response("unused-plan", 0.0));
        responses.insert(BACKEND_BUILDER_ID.into(), ok_response("unused-build", 0.0));
        responses.insert(BACKEND_REVIEWER_ID.into(), ok_verdict_response(0.0));
        responses.insert(TESTER_ID.into(), ok_verdict_response(0.0));

        let mock = Arc::new(MockTransport::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcTransport(Arc::clone(&mock)),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-research-only".into(), "explain X".into())
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 2);
        assert_eq!(outcome.stages[0].state, JobState::Scout);
        assert_eq!(outcome.stages[1].state, JobState::Classify);
        assert_eq!(
            outcome.stages[1]
                .coordinator_decision
                .as_ref()
                .map(|d| d.route),
            Some(CoordinatorRoute::ResearchOnly)
        );
        // No verdict-bearing stage on this path.
        assert!(outcome.last_verdict.is_none());
        // Plan/Build/Review/Test never invoked — the mock recorded
        // exactly two calls (scout + coordinator).
        let seen = mock.seen();
        let plan_calls = seen.iter().filter(|(id, _)| id == PLANNER_ID).count();
        let build_calls = seen.iter().filter(|(id, _)| id == BACKEND_BUILDER_ID).count();
        let review_calls = seen.iter().filter(|(id, _)| id == BACKEND_REVIEWER_ID).count();
        let test_calls = seen.iter().filter(|(id, _)| id == TESTER_ID).count();
        assert_eq!(plan_calls, 0, "ResearchOnly must not invoke Planner");
        assert_eq!(build_calls, 0, "ResearchOnly must not invoke Builder");
        assert_eq!(review_calls, 0, "ResearchOnly must not invoke Reviewer");
        assert_eq!(test_calls, 0, "ResearchOnly must not invoke Tester");
    }

    /// Mock-driven execute-plan path: Coordinator brain returns
    /// `route: execute_plan`; FSM walks the canonical 5-stage chain.
    /// Stage count = 6 (Scout + Classify + Plan + Build + Review + Test).
    #[tokio::test]
    async fn fsm_classify_execute_plan_continues_full_chain() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let responses = happy_5_stage_responses(
            "scout-out", "plan-out", "build-out", 0.01, 0.02, 0.03,
        );
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-execute-plan".into(), "add X".into())
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 6);
        assert_eq!(outcome.stages[1].state, JobState::Classify);
        assert_eq!(
            outcome.stages[1]
                .coordinator_decision
                .as_ref()
                .map(|d| d.route),
            Some(CoordinatorRoute::ExecutePlan)
        );
    }

    /// Coordinator output is unparseable (not JSON, not fenced, not
    /// a balanced object). Per WP §"Notes/risks", the FSM must
    /// default-fail-open to ExecutePlan with a fallback decision
    /// stamped on the StageResult.
    #[tokio::test]
    async fn fsm_classify_unparseable_falls_back_to_execute_plan() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses = happy_5_stage_responses(
            "scout", "plan", "build", 0.0, 0.0, 0.0,
        );
        responses.insert(
            COORDINATOR_ID.into(),
            ok_response("garbage non-JSON output", 0.0),
        );
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(app.handle(), "ws-fallback".into(), "g".into())
            .await
            .expect("FSM ok");
        // Fallback fires → ExecutePlan branch → full 6-stage chain.
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 6);
        let classify = &outcome.stages[1];
        assert_eq!(classify.state, JobState::Classify);
        let decision =
            classify.coordinator_decision.as_ref().expect("fallback decision");
        assert_eq!(decision.route, CoordinatorRoute::ExecutePlan);
        assert!(
            decision.reasoning.contains("fallback"),
            "fallback reasoning should mention fallback: {}",
            decision.reasoning
        );
    }

    /// `DecisionMade` event fires after `StageCompleted(Classify)` and
    /// before the next `StageStarted(Plan)`. On the ExecutePlan
    /// branch the order is: ... → StageStarted(Classify) →
    /// StageCompleted(Classify) → DecisionMade → StageStarted(Plan)
    /// → ...
    #[tokio::test]
    async fn decision_made_event_fires_after_classify() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let responses = happy_5_stage_responses(
            "s", "p", "b", 0.0, 0.0, 0.0,
        );
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-decision-event".to_string();
        let events = capture_events(&app, &job_id);
        fsm.run_job_with_id(
            app.handle(),
            job_id.clone(),
            "ws-decision-event".into(),
            "g".into(),
        )
        .await
        .expect("FSM ok");

        wait_for_events(&events, Duration::from_secs(2), |evts| {
            evts.iter().any(|e| e.kind == KIND_FINISHED)
        })
        .await;

        let captured = events.lock().expect("events poisoned").clone();
        // Locate the indices of the events we care about. The order
        // must be: StageCompleted(Classify) → DecisionMade →
        // StageStarted(Plan).
        let classify_completed_idx = captured
            .iter()
            .position(|e| {
                e.kind == KIND_STAGE_COMPLETED
                    && e.json
                        .get("stage")
                        .and_then(|s| s.get("state"))
                        .and_then(|v| v.as_str())
                        == Some("classify")
            })
            .expect("StageCompleted(Classify) present");
        let decision_idx = captured
            .iter()
            .position(|e| e.kind == KIND_DECISION_MADE)
            .expect("DecisionMade present");
        let plan_started_idx = captured
            .iter()
            .position(|e| {
                e.kind == KIND_STAGE_STARTED
                    && e.json.get("state").and_then(|v| v.as_str())
                        == Some("plan")
            })
            .expect("StageStarted(Plan) present");
        assert!(
            classify_completed_idx < decision_idx,
            "DecisionMade must fire AFTER StageCompleted(Classify)"
        );
        assert!(
            decision_idx < plan_started_idx,
            "DecisionMade must fire BEFORE StageStarted(Plan)"
        );
        // DecisionMade exactly once per job.
        let decision_count = captured
            .iter()
            .filter(|e| e.kind == KIND_DECISION_MADE)
            .count();
        assert_eq!(decision_count, 1);
        // The event payload carries a parseable `decision.route`.
        let decision_event = &captured[decision_idx];
        let route = decision_event
            .json
            .get("decision")
            .and_then(|d| d.get("route"))
            .and_then(|v| v.as_str());
        assert_eq!(route, Some("execute_plan"));
    }

    /// `StageResult.coordinator_decision` round-trips through SQLite.
    /// Drive a full mock-driven run against a pool-backed registry,
    /// reload via `get_job_detail`, assert the Classify stage's
    /// decision survived.
    #[tokio::test]
    async fn coordinator_decision_persists_on_stage_result() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let (registry, _pool, _reg_dir) = pool_backed_registry().await;
        let responses = happy_5_stage_responses(
            "s", "p", "b", 0.0, 0.0, 0.0,
        );
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-decision-persist".to_string();
        fsm.run_job_with_id(
            app.handle(),
            job_id.clone(),
            "ws-decision-persist".into(),
            "g".into(),
        )
        .await
        .expect("FSM ok");

        let detail = registry
            .get_job_detail(&job_id)
            .await
            .expect("query")
            .expect("Some");
        assert_eq!(detail.stages.len(), 6);
        // Stages[1] must be Classify with the ExecutePlan decision.
        let classify = &detail.stages[1];
        assert_eq!(classify.state, JobState::Classify);
        let decision = classify
            .coordinator_decision
            .as_ref()
            .expect("decision persisted");
        assert_eq!(decision.route, CoordinatorRoute::ExecutePlan);
        // Every other stage's decision is None.
        for (i, stage) in detail.stages.iter().enumerate() {
            if i == 1 {
                continue;
            }
            assert!(
                stage.coordinator_decision.is_none(),
                "stage {i} ({:?}) should have no decision",
                stage.state
            );
        }
    }

    /// W3-12g — Classify stage carries the parsed `scope` field.
    /// Mock Coordinator returns `scope=frontend`; the FSM stamps it
    /// onto `stages[1].coordinator_decision.scope` verbatim. W3-12h
    /// also dispatches the frontend chain for this scope, so the
    /// frontend specialists are seeded alongside the backend ones.
    #[tokio::test]
    async fn fsm_classify_emits_scope_in_decision() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses = happy_5_stage_responses(
            "s", "p", "b", 0.0, 0.0, 0.0,
        );
        // Override the coordinator response with a frontend-scoped one.
        responses.insert(
            COORDINATOR_ID.into(),
            execute_plan_frontend_decision_response(),
        );
        // W3-12h: scope=Frontend now dispatches the frontend chain;
        // seed those specialists so the FSM walks the full chain.
        responses
            .insert(FRONTEND_BUILDER_ID.into(), ok_response("fe-build", 0.0));
        responses
            .insert(FRONTEND_REVIEWER_ID.into(), ok_verdict_response(0.0));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(
                app.handle(),
                "ws-scope-emit".into(),
                "rebuild SwarmJobDetail".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        // Stages[1] is Classify; its decision must carry scope=Frontend.
        let classify = &outcome.stages[1];
        assert_eq!(classify.state, JobState::Classify);
        let decision = classify
            .coordinator_decision
            .as_ref()
            .expect("classify decision present");
        assert_eq!(decision.route, CoordinatorRoute::ExecutePlan);
        assert_eq!(decision.scope, CoordinatorScope::Frontend);
    }

    // ----------------------------------------------------------------
    // WP-W3-12h — scope-aware single-domain dispatch
    // ----------------------------------------------------------------

    /// W3-12i: `select_chain_pairs(Backend)` returns one pair —
    /// the backend builder + reviewer.
    #[test]
    fn select_chain_pairs_backend_returns_one_pair() {
        let pairs = select_chain_pairs(CoordinatorScope::Backend);
        assert_eq!(pairs.len(), 1);
        let (b, r) = pairs[0];
        assert_eq!(b, BACKEND_BUILDER_ID);
        assert_eq!(b, "backend-builder");
        assert_eq!(r, BACKEND_REVIEWER_ID);
        assert_eq!(r, "backend-reviewer");
    }

    /// W3-12i: `select_chain_pairs(Frontend)` returns one pair —
    /// the frontend builder + reviewer.
    #[test]
    fn select_chain_pairs_frontend_returns_one_pair() {
        let pairs = select_chain_pairs(CoordinatorScope::Frontend);
        assert_eq!(pairs.len(), 1);
        let (b, r) = pairs[0];
        assert_eq!(b, FRONTEND_BUILDER_ID);
        assert_eq!(b, "frontend-builder");
        assert_eq!(r, FRONTEND_REVIEWER_ID);
        assert_eq!(r, "frontend-reviewer");
    }

    /// W3-12i: `select_chain_pairs(Fullstack)` returns two pairs in
    /// dispatch order — backend pair first (idx 0), then frontend
    /// pair (idx 1). The order is the contract; the FSM run loop
    /// iterates this Vec sequentially.
    #[test]
    fn select_chain_pairs_fullstack_returns_two_pairs() {
        let pairs = select_chain_pairs(CoordinatorScope::Fullstack);
        assert_eq!(pairs.len(), 2);
        // Backend pair MUST come first.
        assert_eq!(pairs[0].0, BACKEND_BUILDER_ID);
        assert_eq!(pairs[0].1, BACKEND_REVIEWER_ID);
        // Frontend pair second.
        assert_eq!(pairs[1].0, FRONTEND_BUILDER_ID);
        assert_eq!(pairs[1].1, FRONTEND_REVIEWER_ID);
    }

    /// scope=Backend (default) keeps the existing dispatch shape:
    /// stages[3]=backend-builder, stages[4]=backend-reviewer.
    /// This is regression coverage — backend chain behavior is
    /// preserved across the W3-12h refactor.
    #[tokio::test]
    async fn fsm_scope_backend_dispatches_backend_chain() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let responses = happy_5_stage_responses(
            "s", "p", "b", 0.0, 0.0, 0.0,
        );
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(
                app.handle(),
                "ws-scope-be-dispatch".into(),
                "backend goal".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 6);
        assert_eq!(outcome.stages[3].state, JobState::Build);
        assert_eq!(outcome.stages[3].specialist_id, BACKEND_BUILDER_ID);
        assert_eq!(outcome.stages[3].specialist_id, "backend-builder");
        assert_eq!(outcome.stages[4].state, JobState::Review);
        assert_eq!(outcome.stages[4].specialist_id, BACKEND_REVIEWER_ID);
        assert_eq!(outcome.stages[4].specialist_id, "backend-reviewer");
    }

    /// W3-12h activation: scope=Frontend dispatches the frontend
    /// chain. Mock Coordinator returns scope=Frontend; assert
    /// stages[3].specialist_id == "frontend-builder" and
    /// stages[4].specialist_id == "frontend-reviewer". (Stage
    /// indices: 0=Scout, 1=Classify, 2=Plan, 3=Build, 4=Review,
    /// 5=Test.)
    #[tokio::test]
    async fn fsm_scope_frontend_dispatches_frontend_chain() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses = happy_5_stage_responses(
            "s", "p", "b", 0.0, 0.0, 0.0,
        );
        responses.insert(
            COORDINATOR_ID.into(),
            execute_plan_frontend_decision_response(),
        );
        // Seed the frontend specialists; the backend ones from the
        // happy-path helper stay unused on this run but cause no
        // harm (MockTransport ignores keys it never sees).
        responses
            .insert(FRONTEND_BUILDER_ID.into(), ok_response("fe-build", 0.0));
        responses
            .insert(FRONTEND_REVIEWER_ID.into(), ok_verdict_response(0.0));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(
                app.handle(),
                "ws-scope-fe-dispatch".into(),
                "frontend goal".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 6);
        // Frontend chain activation — Build + Review specialists
        // must be the frontend variants.
        assert_eq!(outcome.stages[3].state, JobState::Build);
        assert_eq!(outcome.stages[3].specialist_id, FRONTEND_BUILDER_ID);
        assert_eq!(outcome.stages[3].specialist_id, "frontend-builder");
        assert_eq!(outcome.stages[4].state, JobState::Review);
        assert_eq!(outcome.stages[4].specialist_id, FRONTEND_REVIEWER_ID);
        assert_eq!(outcome.stages[4].specialist_id, "frontend-reviewer");
        // Decision carries scope=Frontend.
        assert_eq!(
            outcome.stages[1]
                .coordinator_decision
                .as_ref()
                .map(|d| d.scope),
            Some(CoordinatorScope::Frontend)
        );
    }

    // ----------------------------------------------------------------
    // WP-W3-12i — Fullstack sequential dispatch (BB+BR then FB+FR)
    // ----------------------------------------------------------------

    /// Build the canonical 8-stage Fullstack happy-path response set:
    /// scout/planner/builders return text; reviewers + tester return
    /// approved verdicts; coordinator votes ExecutePlan + Fullstack.
    /// Used by Fullstack happy-path tests so each test doesn't have
    /// to spell out 8 mock entries inline.
    fn happy_fullstack_responses() -> HashMap<String, MockResponse> {
        let mut r: HashMap<String, MockResponse> = HashMap::new();
        r.insert(SCOUT_ID.into(), ok_response("scout findings", 0.0));
        r.insert(PLANNER_ID.into(), ok_response("dual-domain plan", 0.0));
        r.insert(BACKEND_BUILDER_ID.into(), ok_response("be-build", 0.0));
        r.insert(BACKEND_REVIEWER_ID.into(), ok_verdict_response(0.0));
        r.insert(FRONTEND_BUILDER_ID.into(), ok_response("fe-build", 0.0));
        r.insert(FRONTEND_REVIEWER_ID.into(), ok_verdict_response(0.0));
        r.insert(TESTER_ID.into(), ok_verdict_response(0.0));
        r.insert(
            COORDINATOR_ID.into(),
            execute_plan_fullstack_decision_response(),
        );
        r
    }

    /// scope=Fullstack walks all 8 stages on the approved path:
    /// scout → coordinator (Classify) → planner → backend-builder →
    /// backend-reviewer → frontend-builder → frontend-reviewer →
    /// integration-tester → Done. Stage IDs assert the dispatch
    /// order is exactly the report §2.1 sequential hierarchy.
    #[tokio::test]
    async fn fsm_scope_fullstack_walks_eight_stages_on_approved_path() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(happy_fullstack_responses()),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        let outcome = fsm
            .run_job(
                app.handle(),
                "ws-fs-happy".into(),
                "add /me IPC + useMe hook".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 8);
        // Stage IDs in order — this is the contract.
        let expected_ids = [
            SCOUT_ID,
            COORDINATOR_ID,
            PLANNER_ID,
            BACKEND_BUILDER_ID,
            BACKEND_REVIEWER_ID,
            FRONTEND_BUILDER_ID,
            FRONTEND_REVIEWER_ID,
            TESTER_ID,
        ];
        for (idx, expected) in expected_ids.iter().enumerate() {
            assert_eq!(
                outcome.stages[idx].specialist_id, *expected,
                "stage {idx}: expected `{expected}`, got `{}`",
                outcome.stages[idx].specialist_id,
            );
        }
        // Stage state ordering: scout, classify, plan, build, review,
        // build, review, test.
        assert_eq!(outcome.stages[0].state, JobState::Scout);
        assert_eq!(outcome.stages[1].state, JobState::Classify);
        assert_eq!(outcome.stages[2].state, JobState::Plan);
        assert_eq!(outcome.stages[3].state, JobState::Build);
        assert_eq!(outcome.stages[4].state, JobState::Review);
        assert_eq!(outcome.stages[5].state, JobState::Build);
        assert_eq!(outcome.stages[6].state, JobState::Review);
        assert_eq!(outcome.stages[7].state, JobState::Test);
        // Both Reviewer stages + the Tester stage carry verdicts.
        for idx in [4, 6, 7] {
            assert!(
                outcome.stages[idx]
                    .verdict
                    .as_ref()
                    .map(|v| v.approved)
                    .unwrap_or(false),
                "stage {idx} should carry approved verdict"
            );
        }
        // Decision still records scope=Fullstack.
        assert_eq!(
            outcome.stages[1]
                .coordinator_decision
                .as_ref()
                .map(|d| d.scope),
            Some(CoordinatorScope::Fullstack)
        );
    }

    /// BackendReviewer rejects on attempt 1, approves on attempt 2.
    /// FrontendReviewer + Tester always approve. Final Done,
    /// retry_count=1.
    ///
    /// Stage count breakdown:
    /// - attempt 1: scout(1) + classify(1) + plan(1) + BB(1) +
    ///   BR-rejected(1) = 5 stages.
    /// - attempt 2: plan(1) + BB(1) + BR-approved(1) + FB(1) +
    ///   FR-approved(1) + test(1) = 6 stages. (Scout + Classify
    ///   cached.)
    /// - total = 11.
    #[tokio::test]
    async fn fsm_scope_fullstack_backend_review_rejection_retries_full_chain() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_fullstack_decision_response()],
        );
        responses.insert(
            PLANNER_ID.into(),
            vec![ok_response("p1", 0.0), ok_response("p2", 0.0)],
        );
        responses.insert(
            BACKEND_BUILDER_ID.into(),
            vec![ok_response("be1", 0.0), ok_response("be2", 0.0)],
        );
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![
                rejected_verdict_response("missing import"),
                ok_verdict_response(0.0),
            ],
        );
        responses
            .insert(FRONTEND_BUILDER_ID.into(), vec![ok_response("fe", 0.0)]);
        responses
            .insert(FRONTEND_REVIEWER_ID.into(), vec![ok_verdict_response(0.0)]);
        responses.insert(TESTER_ID.into(), vec![ok_verdict_response(0.0)]);

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-fs-br-retry".to_string();
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-fs-br-retry".into(),
                "fullstack goal".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 11);
        let job = registry.get(&job_id).expect("job in registry");
        assert_eq!(job.retry_count, 1);
        // Frontend specialists never invoked on attempt 1 (BR
        // short-circuits before FB/FR).
        assert_eq!(mock.call_count(FRONTEND_BUILDER_ID), 1);
        assert_eq!(mock.call_count(FRONTEND_REVIEWER_ID), 1);
        // Backend specialists invoked twice (one per attempt).
        assert_eq!(mock.call_count(BACKEND_BUILDER_ID), 2);
        assert_eq!(mock.call_count(BACKEND_REVIEWER_ID), 2);
        // Scout + Coordinator each once.
        assert_eq!(mock.call_count(SCOUT_ID), 1);
        assert_eq!(mock.call_count(COORDINATOR_ID), 1);
    }

    /// FrontendReviewer rejects on attempt 1, approves on attempt 2.
    /// All other gates approve. Final Done, retry_count=1.
    ///
    /// Stage count breakdown:
    /// - attempt 1: scout + classify + plan + BB + BR-approved +
    ///   FB + FR-rejected = 7 stages.
    /// - attempt 2: plan + BB + BR + FB + FR + test = 6 stages.
    ///   (Scout + Classify cached.)
    /// - total = 13.
    ///
    /// Backend stages re-run on retry — wasteful but correct (per-
    /// domain retry is a future polish).
    #[tokio::test]
    async fn fsm_scope_fullstack_frontend_review_rejection_retries_full_chain() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_fullstack_decision_response()],
        );
        responses.insert(
            PLANNER_ID.into(),
            vec![ok_response("p1", 0.0), ok_response("p2", 0.0)],
        );
        responses.insert(
            BACKEND_BUILDER_ID.into(),
            vec![ok_response("be1", 0.0), ok_response("be2", 0.0)],
        );
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![ok_verdict_response(0.0), ok_verdict_response(0.0)],
        );
        responses.insert(
            FRONTEND_BUILDER_ID.into(),
            vec![ok_response("fe1", 0.0), ok_response("fe2", 0.0)],
        );
        responses.insert(
            FRONTEND_REVIEWER_ID.into(),
            vec![
                rejected_verdict_response("missing JSDoc"),
                ok_verdict_response(0.0),
            ],
        );
        responses.insert(TESTER_ID.into(), vec![ok_verdict_response(0.0)]);

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-fs-fr-retry".to_string();
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-fs-fr-retry".into(),
                "fullstack goal".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 13);
        let job = registry.get(&job_id).expect("job in registry");
        assert_eq!(job.retry_count, 1);
        // Backend stages re-run on retry — cost of full-chain retry.
        assert_eq!(mock.call_count(BACKEND_BUILDER_ID), 2);
        assert_eq!(mock.call_count(BACKEND_REVIEWER_ID), 2);
        // Frontend stages also run twice (one per attempt).
        assert_eq!(mock.call_count(FRONTEND_BUILDER_ID), 2);
        assert_eq!(mock.call_count(FRONTEND_REVIEWER_ID), 2);
    }

    /// Test gate rejects on attempt 1, approves on attempt 2. Both
    /// Reviewer gates always approve. Final Done, retry_count=1.
    /// Re-runs the full chain (scout cached) on retry.
    #[tokio::test]
    async fn fsm_scope_fullstack_test_rejection_retries_full_chain() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_fullstack_decision_response()],
        );
        responses.insert(
            PLANNER_ID.into(),
            vec![ok_response("p1", 0.0), ok_response("p2", 0.0)],
        );
        responses.insert(
            BACKEND_BUILDER_ID.into(),
            vec![ok_response("be1", 0.0), ok_response("be2", 0.0)],
        );
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![ok_verdict_response(0.0), ok_verdict_response(0.0)],
        );
        responses.insert(
            FRONTEND_BUILDER_ID.into(),
            vec![ok_response("fe1", 0.0), ok_response("fe2", 0.0)],
        );
        responses.insert(
            FRONTEND_REVIEWER_ID.into(),
            vec![ok_verdict_response(0.0), ok_verdict_response(0.0)],
        );
        responses.insert(
            TESTER_ID.into(),
            vec![
                rejected_verdict_response("test_foo failed"),
                ok_verdict_response(0.0),
            ],
        );

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-fs-test-retry".to_string();
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-fs-test-retry".into(),
                "fullstack goal".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        let job = registry.get(&job_id).expect("job");
        assert_eq!(job.retry_count, 1);
        // attempt 1: scout + classify + plan + BB + BR + FB + FR +
        // test-rejected = 8. attempt 2: plan + BB + BR + FB + FR +
        // test = 6. Total 14.
        assert_eq!(outcome.stages.len(), 14);
        // Tester invoked twice; both Reviewer gates twice; Builders
        // twice each.
        assert_eq!(mock.call_count(TESTER_ID), 2);
        assert_eq!(mock.call_count(BACKEND_BUILDER_ID), 2);
        assert_eq!(mock.call_count(FRONTEND_BUILDER_ID), 2);
    }

    /// FrontendReviewer rejects on every attempt → retry budget
    /// exhausts at MAX_RETRIES. Final Failed, retry_count=2,
    /// last_verdict.approved=false.
    #[tokio::test]
    async fn fsm_scope_fullstack_exhausts_retries_finalizes_failed() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_fullstack_decision_response()],
        );
        responses.insert(PLANNER_ID.into(), vec![ok_response("p", 0.0)]);
        responses.insert(BACKEND_BUILDER_ID.into(), vec![ok_response("be", 0.0)]);
        responses.insert(
            BACKEND_REVIEWER_ID.into(),
            vec![ok_verdict_response(0.0)],
        );
        responses.insert(FRONTEND_BUILDER_ID.into(), vec![ok_response("fe", 0.0)]);
        responses.insert(
            FRONTEND_REVIEWER_ID.into(),
            vec![rejected_verdict_response("never approving")],
        );
        responses.insert(TESTER_ID.into(), vec![ok_verdict_response(0.0)]);

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-fs-exhaust".to_string();
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-fs-exhaust".into(),
                "fullstack goal".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Failed);
        let job = registry.get(&job_id).expect("job");
        assert_eq!(job.retry_count, MAX_RETRIES);
        // last_verdict reflects the rejecting FrontendReviewer.
        let lv = outcome.last_verdict.as_ref().expect("last_verdict set");
        assert!(!lv.approved);
        // Tester never runs across all attempts (FR rejects first).
        assert_eq!(mock.call_count(TESTER_ID), 0);
    }

    /// Plan prompt for scope=Fullstack carries the fullstack scope
    /// label so the Planner produces a dual-domain plan.
    #[test]
    fn plan_prompt_includes_fullstack_scope_when_scope_is_fullstack() {
        let p = render_plan_prompt(
            "G",
            "S",
            CoordinatorScope::Fullstack,
        );
        assert!(
            p.contains("Kapsam: fullstack"),
            "fullstack scope not in plan prompt: {p}"
        );
    }

    /// Plan prompt for scope=Backend carries the backend scope label.
    #[test]
    fn plan_prompt_includes_backend_scope_when_scope_is_backend() {
        let p = render_plan_prompt("G", "S", CoordinatorScope::Backend);
        assert!(
            p.contains("Kapsam: backend"),
            "backend scope not in plan prompt: {p}"
        );
    }

    /// Plan prompt for scope=Frontend carries the frontend scope label.
    #[test]
    fn plan_prompt_includes_frontend_scope_when_scope_is_frontend() {
        let p = render_plan_prompt("G", "S", CoordinatorScope::Frontend);
        assert!(
            p.contains("Kapsam: frontend"),
            "frontend scope not in plan prompt: {p}"
        );
    }

    /// Retry plan prompt also carries the scope so retries stay
    /// scope-aware (Fullstack retries must still cover both domains).
    #[test]
    fn retry_plan_prompt_carries_scope() {
        let verdict = Verdict {
            approved: false,
            issues: vec![],
            summary: "no good".into(),
        };
        let p = render_retry_plan_prompt(
            "G",
            "S",
            "PREV",
            &verdict,
            JobState::Review,
            CoordinatorScope::Fullstack,
        );
        assert!(
            p.contains("Kapsam: fullstack"),
            "retry plan prompt missing fullstack scope: {p}"
        );
        // Sanity: scope substitution leaves no placeholders.
        assert!(!p.contains("{scope}"));
    }

    /// On a Fullstack run, the BackendBuilder's BUILD prompt carries
    /// the backend domain note (steers Builder toward backend steps
    /// of the dual-domain plan).
    #[tokio::test]
    async fn build_prompt_includes_backend_domain_note_for_backend_builder() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mock = Arc::new(MockTransport::new(happy_fullstack_responses()));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcTransport(Arc::clone(&mock)),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        fsm.run_job(app.handle(), "ws-fs-be-note".into(), "g".into())
            .await
            .expect("FSM ok");
        let seen = mock.seen();
        let be_build_prompt = seen
            .iter()
            .find(|(id, _)| id == BACKEND_BUILDER_ID)
            .map(|(_, p)| p.as_str())
            .expect("backend-builder prompt recorded");
        assert!(
            be_build_prompt.contains("backend tarafına bakıyorsun"),
            "backend-builder prompt missing backend domain note: {be_build_prompt}"
        );
    }

    /// On a Fullstack run, the FrontendBuilder's BUILD prompt carries
    /// the frontend domain note.
    #[tokio::test]
    async fn build_prompt_includes_frontend_domain_note_for_frontend_builder() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mock = Arc::new(MockTransport::new(happy_fullstack_responses()));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcTransport(Arc::clone(&mock)),
            Arc::new(JobRegistry::new()),
            Duration::from_secs(5),
        );
        fsm.run_job(app.handle(), "ws-fs-fe-note".into(), "g".into())
            .await
            .expect("FSM ok");
        let seen = mock.seen();
        let fe_build_prompt = seen
            .iter()
            .find(|(id, _)| id == FRONTEND_BUILDER_ID)
            .map(|(_, p)| p.as_str())
            .expect("frontend-builder prompt recorded");
        assert!(
            fe_build_prompt.contains("frontend tarafına bakıyorsun"),
            "frontend-builder prompt missing frontend domain note: {fe_build_prompt}"
        );
    }

    /// Pool-backed Fullstack happy path. All 8 stages and per-stage
    /// `specialist_id` round-trip through SQLite via the registry's
    /// write-through.
    #[tokio::test]
    async fn fsm_fullstack_persistence_round_trip() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let (registry, _pool, _reg_dir) = pool_backed_registry().await;
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(happy_fullstack_responses()),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-fs-persist".to_string();
        fsm.run_job_with_id(
            app.handle(),
            job_id.clone(),
            "ws-fs-persist".into(),
            "fullstack goal".into(),
        )
        .await
        .expect("FSM ok");

        let detail = registry
            .get_job_detail(&job_id)
            .await
            .expect("query")
            .expect("Some");
        assert_eq!(detail.stages.len(), 8);
        // Per-stage specialist_id round-trip in dispatch order.
        assert_eq!(detail.stages[0].specialist_id, SCOUT_ID);
        assert_eq!(detail.stages[1].specialist_id, COORDINATOR_ID);
        assert_eq!(detail.stages[2].specialist_id, PLANNER_ID);
        assert_eq!(detail.stages[3].specialist_id, BACKEND_BUILDER_ID);
        assert_eq!(detail.stages[4].specialist_id, BACKEND_REVIEWER_ID);
        assert_eq!(detail.stages[5].specialist_id, FRONTEND_BUILDER_ID);
        assert_eq!(detail.stages[6].specialist_id, FRONTEND_REVIEWER_ID);
        assert_eq!(detail.stages[7].specialist_id, TESTER_ID);
        // Decision still records scope=Fullstack.
        let classify = &detail.stages[1];
        let decision = classify
            .coordinator_decision
            .as_ref()
            .expect("decision persisted");
        assert_eq!(decision.scope, CoordinatorScope::Fullstack);
    }

    /// Real-claude integration smoke (`#[ignore]`) for the Fullstack
    /// chain. Owner runs it manually with `cargo test -- --ignored`
    /// post-commit. The Coordinator brain should classify a
    /// dual-domain goal as `scope=fullstack`; the FSM dispatches all
    /// 8 stages.
    ///
    /// Time budget: 8 stages × 180s = 1440s (24 min) worst-case;
    /// typical 6-12 min once warm.
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_fullstack_chain_real_claude() {
        use crate::swarm::transport::SubprocessTransport;

        let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(180);

        let (app, _pool, _dir) = mock_app_with_pool().await;
        let profiles = bundled_registry();
        let transport = SubprocessTransport::new();
        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            profiles,
            transport,
            registry,
            Duration::from_secs(stage_secs),
        );
        // Textbook fullstack edit: one Rust file, one TSX file,
        // both doc-only.
        let goal = "Add a one-line doc comment to TWO files: \
            (1) above the `Job` struct definition in \
            src-tauri/src/swarm/coordinator/job.rs, briefly noting that \
            Job carries the full lifecycle of a swarm run; \
            (2) above the `formatRelativeMs` function exported from \
            app/src/components/SwarmJobList.tsx, briefly noting that \
            the helper rounds to the nearest minute. \
            Just the two doc comments. Do not change behavior.";
        let outcome = fsm
            .run_job(app.handle(), "default".into(), goal.into())
            .await
            .expect("FSM returns Ok");
        let stage_summary: Vec<(String, String)> = outcome
            .stages
            .iter()
            .map(|s| (format!("{:?}", s.state), s.specialist_id.clone()))
            .collect();
        assert_eq!(
            outcome.final_state,
            JobState::Done,
            "expected Done, got {:?} (stages: {:?})",
            outcome.final_state,
            stage_summary,
        );
        assert_eq!(outcome.stages.len(), 8);
        let expected_ids = [
            "scout",
            "coordinator",
            "planner",
            "backend-builder",
            "backend-reviewer",
            "frontend-builder",
            "frontend-reviewer",
            "integration-tester",
        ];
        for (idx, expected) in expected_ids.iter().enumerate() {
            assert_eq!(
                outcome.stages[idx].specialist_id, *expected,
                "stage {idx}: expected specialist_id `{expected}`, got `{}`",
                outcome.stages[idx].specialist_id,
            );
        }
        let classify_stage = &outcome.stages[1];
        let decision = classify_stage
            .coordinator_decision
            .as_ref()
            .expect("Classify stage must carry a CoordinatorDecision");
        assert_eq!(decision.scope, CoordinatorScope::Fullstack);
    }

    /// `StageResult.specialist_id` round-trips through SQLite for a
    /// frontend-scope job. Drive the FSM against a pool-backed
    /// registry with `scope=frontend`; reload via
    /// `get_job_detail` and assert the per-stage `specialist_id`s
    /// match the frontend chain.
    #[tokio::test]
    async fn fsm_frontend_chain_persists_correct_specialist_ids() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let (registry, _pool, _reg_dir) = pool_backed_registry().await;
        let mut responses = happy_5_stage_responses(
            "s", "p", "b", 0.0, 0.0, 0.0,
        );
        responses.insert(
            COORDINATOR_ID.into(),
            execute_plan_frontend_decision_response(),
        );
        responses
            .insert(FRONTEND_BUILDER_ID.into(), ok_response("fe-build", 0.0));
        responses
            .insert(FRONTEND_REVIEWER_ID.into(), ok_verdict_response(0.0));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            MockTransport::new(responses),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );
        let job_id = "j-fe-persist".to_string();
        fsm.run_job_with_id(
            app.handle(),
            job_id.clone(),
            "ws-fe-persist".into(),
            "frontend goal".into(),
        )
        .await
        .expect("FSM ok");

        let detail = registry
            .get_job_detail(&job_id)
            .await
            .expect("query")
            .expect("Some");
        assert_eq!(detail.stages.len(), 6);
        // Per-stage specialist_id round-trip.
        assert_eq!(detail.stages[0].specialist_id, SCOUT_ID);
        assert_eq!(detail.stages[1].specialist_id, COORDINATOR_ID);
        assert_eq!(detail.stages[2].specialist_id, PLANNER_ID);
        assert_eq!(detail.stages[3].specialist_id, FRONTEND_BUILDER_ID);
        assert_eq!(detail.stages[4].specialist_id, FRONTEND_REVIEWER_ID);
        assert_eq!(detail.stages[5].specialist_id, TESTER_ID);
    }

    /// On a frontend Reviewer rejection + retry, BOTH the first and
    /// the retried attempt dispatch the frontend chain (no accidental
    /// fall-back to backend on retry). The chain selection lives
    /// inside the retry loop body, so each iteration re-evaluates
    /// `decision.scope` — but `decision` is stable per job, so every
    /// iteration must pick the same frontend pair.
    #[tokio::test]
    async fn fsm_frontend_retry_loop_preserves_chain_choice() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());

        let mut responses: HashMap<String, Vec<MockResponse>> = HashMap::new();
        responses.insert(SCOUT_ID.into(), vec![ok_response("s", 0.0)]);
        responses.insert(
            COORDINATOR_ID.into(),
            vec![execute_plan_frontend_decision_response()],
        );
        responses.insert(
            PLANNER_ID.into(),
            vec![ok_response("p1", 0.0), ok_response("p2", 0.0)],
        );
        responses.insert(
            FRONTEND_BUILDER_ID.into(),
            vec![ok_response("fe-b1", 0.0), ok_response("fe-b2", 0.0)],
        );
        responses.insert(
            FRONTEND_REVIEWER_ID.into(),
            vec![
                rejected_verdict_response("missing JSDoc"),
                ok_verdict_response(0.0),
            ],
        );
        responses.insert(TESTER_ID.into(), vec![ok_verdict_response(0.0)]);

        let mock = Arc::new(SequencedMock::new(responses));
        let fsm = CoordinatorFsm::new(
            synthetic_registry(),
            ArcSequencedMock(Arc::clone(&mock)),
            Arc::clone(&registry),
            Duration::from_secs(5),
        );

        let job_id = "j-fe-retry".to_string();
        let outcome = fsm
            .run_job_with_id(
                app.handle(),
                job_id.clone(),
                "ws-fe-retry".into(),
                "frontend goal".into(),
            )
            .await
            .expect("FSM ok");
        assert_eq!(outcome.final_state, JobState::Done);
        // 1 scout + 1 classify + 2 × (plan/build/review) + 1 test = 9.
        assert_eq!(outcome.stages.len(), 9);

        // FrontendBuilder + FrontendReviewer each invoked exactly
        // twice (attempt 1 + retry). NO backend dispatches.
        assert_eq!(mock.call_count(FRONTEND_BUILDER_ID), 2);
        assert_eq!(mock.call_count(FRONTEND_REVIEWER_ID), 2);
        assert_eq!(mock.call_count(BACKEND_BUILDER_ID), 0);
        assert_eq!(mock.call_count(BACKEND_REVIEWER_ID), 0);

        // Both Build stages and both Review stages must report the
        // frontend specialist ids.
        let build_stages: Vec<&StageResult> = outcome
            .stages
            .iter()
            .filter(|s| s.state == JobState::Build)
            .collect();
        assert_eq!(build_stages.len(), 2);
        for s in &build_stages {
            assert_eq!(s.specialist_id, FRONTEND_BUILDER_ID);
        }
        let review_stages: Vec<&StageResult> = outcome
            .stages
            .iter()
            .filter(|s| s.state == JobState::Review)
            .collect();
        assert_eq!(review_stages.len(), 2);
        for s in &review_stages {
            assert_eq!(s.specialist_id, FRONTEND_REVIEWER_ID);
        }
    }

    /// Real-claude integration smoke (`#[ignore]`) for the frontend
    /// chain. Owner runs it manually with `cargo test -- --ignored`
    /// post-commit. The Coordinator brain should classify a
    /// frontend-targeted goal as `scope=frontend`; the FSM should
    /// dispatch FrontendBuilder + FrontendReviewer.
    ///
    /// Time budget: 6 stages × 180s = 1080s worst-case; typical 60-180s.
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_frontend_chain_real_claude() {
        use crate::swarm::transport::SubprocessTransport;

        let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(180);

        let (app, _pool, _dir) = mock_app_with_pool().await;
        let profiles = bundled_registry();
        let transport = SubprocessTransport::new();
        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            profiles,
            transport,
            registry,
            Duration::from_secs(stage_secs),
        );
        // Frontend-targeted goal — minimal, low-risk doc edit on a
        // TSX file. Doc-only edit avoids provoking IntegrationTester
        // into a full `pnpm test` build.
        let goal = "Add a one-line JSDoc comment above the \
            `formatRelativeMs` helper exported from \
            app/src/components/SwarmJobList.tsx explaining that the helper \
            rounds to the nearest minute. Just the doc comment. \
            Do not add tests, do not change behavior.";
        let outcome = fsm
            .run_job(app.handle(), "default".into(), goal.into())
            .await
            .expect("FSM returns Ok");
        let stage_summary: Vec<(String, String)> = outcome
            .stages
            .iter()
            .map(|s| (format!("{:?}", s.state), s.specialist_id.clone()))
            .collect();
        assert_eq!(
            outcome.final_state,
            JobState::Done,
            "expected Done, got {:?} (last_error: {:?}, last_verdict: {:?}, stages: {:?})",
            outcome.final_state,
            outcome.last_error,
            outcome.last_verdict,
            stage_summary,
        );
        // Build + Review specialists must be the frontend variants.
        let build_stage = outcome
            .stages
            .iter()
            .find(|s| s.state == JobState::Build)
            .expect("Build stage present");
        assert_eq!(
            build_stage.specialist_id,
            FRONTEND_BUILDER_ID,
            "expected frontend-builder, got {} (scope={:?})",
            build_stage.specialist_id,
            outcome.stages[1]
                .coordinator_decision
                .as_ref()
                .map(|d| d.scope)
        );
        let review_stage = outcome
            .stages
            .iter()
            .find(|s| s.state == JobState::Review)
            .expect("Review stage present");
        assert_eq!(review_stage.specialist_id, FRONTEND_REVIEWER_ID);
    }

    /// `render_classify_prompt` substitutes the goal and scout
    /// findings into the Turkish template.
    #[test]
    fn classify_prompt_includes_goal_and_scout_findings() {
        let goal = "EXACT-GOAL-MARKER";
        let scout_output = "EXACT-SCOUT-MARKER\nfile:line — finding";
        let rendered = render_classify_prompt(goal, scout_output);
        assert!(
            rendered.contains(goal),
            "rendered prompt missing goal: {rendered}"
        );
        assert!(
            rendered.contains(scout_output),
            "rendered prompt missing scout output: {rendered}"
        );
        // Turkish header lands.
        assert!(
            rendered.contains("Hedef:"),
            "rendered prompt missing Turkish header: {rendered}"
        );
        // OUTPUT CONTRACT restated inline so the LLM hits the JSON
        // path even when the persona body is long.
        assert!(
            rendered.contains("research_only"),
            "rendered prompt missing route option: {rendered}"
        );
        assert!(
            rendered.contains("execute_plan"),
            "rendered prompt missing route option: {rendered}"
        );
        // No leftover placeholders.
        assert!(
            !rendered.contains("{goal}"),
            "rendered prompt has unsubstituted {{goal}}: {rendered}"
        );
        assert!(
            !rendered.contains("{scout_output}"),
            "rendered prompt has unsubstituted {{scout_output}}: {rendered}"
        );
    }

    /// `next_state` handles the new Classify state. Both edges land:
    /// Ok → Plan (assumes ExecutePlan; the run loop owns the
    /// ResearchOnly short-circuit), Err → Failed.
    #[test]
    fn next_state_classify_transitions() {
        assert_eq!(next_state(JobState::Scout, true), JobState::Classify);
        assert_eq!(next_state(JobState::Classify, true), JobState::Plan);
        assert_eq!(next_state(JobState::Classify, false), JobState::Failed);
    }

    /// Real-claude integration smoke (`#[ignore]`) — drives a
    /// research-only goal end-to-end. Owner runs it manually with
    /// `cargo test -- --ignored` post-commit.
    ///
    /// Time budget: 2 × 180s = 360s worst-case (Scout + Classify);
    /// typical 30-60s. The Coordinator brain should pick
    /// `research_only` for an "explain how X works" prompt.
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_research_only_real_claude() {
        use crate::swarm::transport::SubprocessTransport;

        let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(180);

        let (app, _pool, _dir) = mock_app_with_pool().await;
        let profiles = bundled_registry();
        let transport = SubprocessTransport::new();
        let registry = Arc::new(JobRegistry::new());
        let fsm = CoordinatorFsm::new(
            profiles,
            transport,
            registry,
            Duration::from_secs(stage_secs),
        );
        let goal = "Explain how the FSM transitions work in \
            src-tauri/src/swarm/coordinator/fsm.rs based on the \
            next_state function.";
        let outcome = fsm
            .run_job(app.handle(), "default".into(), goal.into())
            .await
            .expect("FSM returns Ok");
        assert_eq!(
            outcome.final_state,
            JobState::Done,
            "expected Done, got {:?} (last_error: {:?})",
            outcome.final_state,
            outcome.last_error
        );
        // ResearchOnly short-circuit → Scout + Classify only.
        assert_eq!(outcome.stages.len(), 2);
        let decision = outcome
            .stages[1]
            .coordinator_decision
            .as_ref()
            .expect("Classify stage carries a decision");
        assert_eq!(
            decision.route,
            CoordinatorRoute::ResearchOnly,
            "Coordinator brain should classify an 'explain X' goal as ResearchOnly"
        );
    }

    /// `verdict_issues_as_bullets` empty-list sentinel.
    #[test]
    fn verdict_issues_as_bullets_handles_empty() {
        let s = verdict_issues_as_bullets(&[]);
        assert!(
            s.contains("Ayrıntılı"),
            "empty list should fall back to a sentinel: {s}"
        );
    }

    /// `verdict_issues_as_bullets` with a mix of file/line shapes.
    #[test]
    fn verdict_issues_as_bullets_renders_severity_and_location() {
        let issues = vec![
            VerdictIssue {
                severity: VerdictSeverity::High,
                file: Some("a.rs".into()),
                line: Some(10),
                message: "msg-A".into(),
            },
            VerdictIssue {
                severity: VerdictSeverity::Med,
                file: Some("b.rs".into()),
                line: None,
                message: "msg-B".into(),
            },
            VerdictIssue {
                severity: VerdictSeverity::Low,
                file: None,
                line: None,
                message: "msg-C".into(),
            },
        ];
        let s = verdict_issues_as_bullets(&issues);
        assert!(s.contains("[high] a.rs:10: msg-A"));
        assert!(s.contains("[med] b.rs: msg-B"));
        assert!(s.contains("[low] —: msg-C"));
    }

    /// `render_retry_plan_prompt` substitutes goal, scout, prev plan,
    /// gate label, and verdict issues into the template.
    #[test]
    fn render_retry_plan_prompt_substitutes_all_fields() {
        let verdict = Verdict {
            approved: false,
            issues: vec![VerdictIssue {
                severity: VerdictSeverity::High,
                file: None,
                line: None,
                message: "boom".into(),
            }],
            summary: "no good".into(),
        };
        let p = render_retry_plan_prompt(
            "GOAL-X",
            "SCOUT-Y",
            "PREV-Z",
            &verdict,
            JobState::Test,
            CoordinatorScope::Backend,
        );
        assert!(p.contains("GOAL-X"));
        assert!(p.contains("SCOUT-Y"));
        assert!(p.contains("PREV-Z"));
        assert!(p.contains("[high] —: boom"));
        // Test gate renders as "IntegrationTester" (not "test").
        assert!(p.contains("IntegrationTester"));
        // Both occurrences of {gate} got substituted.
        assert!(!p.contains("{gate}"));
    }
}
