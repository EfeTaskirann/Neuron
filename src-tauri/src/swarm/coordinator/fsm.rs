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

use super::job::{
    Job, JobOutcome, JobRegistry, JobState, StageResult, SwarmJobEvent,
};
use super::verdict::{parse_verdict, Verdict};

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
pub const BUILDER_ID: &str = "backend-builder";
/// Reviewer profile id (W3-12d). Runs after `BUILDER_ID` and emits
/// a JSON Verdict the FSM gates on.
pub const REVIEWER_ID: &str = "reviewer";
/// IntegrationTester profile id (W3-12d). Runs after a successful
/// Reviewer verdict; emits a JSON Verdict the FSM gates on for
/// the final transition to `Done`.
pub const TESTER_ID: &str = "integration-tester";

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

/// PLAN stage prompt template — Turkish, exact text from WP §3.
/// Substitutions: `{goal}`, `{scout_output}`.
const PLAN_PROMPT_TEMPLATE: &str = "Hedef: {goal}\n\
\n\
Scout bulguları:\n\
\n\
{scout_output}\n\
\n\
Bu hedef için adım adım bir plan üret.\n";

/// BUILD stage prompt template — Turkish, exact text from WP §3.
/// Substitutions: `{plan_output}`. The "step 1 only" instruction is
/// the contract from the manual mini-flow validation; multi-step
/// build is a W3-12d concern.
const BUILD_PROMPT_TEMPLATE: &str = "Aşağıdaki Plan'ın 1. adımını uygula.\n\
\n\
{plan_output}\n\
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

/// Render the SCOUT prompt by substituting `{goal}`. Free fn so
/// the prompt-template test can call it without a full FSM.
fn render_scout_prompt(goal: &str) -> String {
    SCOUT_PROMPT_TEMPLATE.replace("{goal}", goal)
}

/// Render the PLAN prompt by substituting `{goal}` and
/// `{scout_output}`. Pulled out as a free fn so the prompt-template
/// test can call it without instantiating a full FSM.
fn render_plan_prompt(goal: &str, scout_output: &str) -> String {
    PLAN_PROMPT_TEMPLATE
        .replace("{goal}", goal)
        .replace("{scout_output}", scout_output)
}

/// Render the BUILD prompt by substituting `{plan_output}`.
fn render_build_prompt(plan_output: &str) -> String {
    BUILD_PROMPT_TEMPLATE.replace("{plan_output}", plan_output)
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
        (JobState::Scout, true) => JobState::Plan,
        (JobState::Scout, false) => JobState::Failed,
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
        let builder = self
            .profiles
            .get(BUILDER_ID)
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "swarm profile `{BUILDER_ID}` (required for FSM)"
                ))
            })?
            .clone();
        let reviewer = self
            .profiles
            .get(REVIEWER_ID)
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "swarm profile `{REVIEWER_ID}` (required for FSM)"
                ))
            })?
            .clone();
        let tester = self
            .profiles
            .get(TESTER_ID)
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "swarm profile `{TESTER_ID}` (required for FSM)"
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

        // SCOUT stage.
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

        // PLAN stage.
        let plan_prompt = render_plan_prompt(&goal, &scout_text);
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

        // BUILD stage.
        let build_prompt = render_build_prompt(&plan_text);
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

        // REVIEW stage (W3-12d). Run reviewer, parse Verdict, and
        // gate on `approved`.
        let review_prompt =
            render_review_prompt(&goal, &plan_text, &build_text);
        match self
            .run_verdict_stage(
                app,
                JobState::Review,
                &reviewer,
                &review_prompt,
                &job_id,
                &workspace_id,
                &notify,
            )
            .await?
        {
            VerdictStageOutcome::Approved(_) => {}
            VerdictStageOutcome::Rejected | VerdictStageOutcome::Terminated => {
                return self.emit_finished_and_build(app, &job_id);
            }
        }

        // TEST stage (W3-12d). Same shape as Review.
        let test_prompt = render_test_prompt(&goal, &build_text);
        match self
            .run_verdict_stage(
                app,
                JobState::Test,
                &tester,
                &test_prompt,
                &job_id,
                &workspace_id,
                &notify,
            )
            .await?
        {
            VerdictStageOutcome::Approved(_) => {}
            VerdictStageOutcome::Rejected | VerdictStageOutcome::Terminated => {
                return self.emit_finished_and_build(app, &job_id);
            }
        }

        // 8. Happy path: mark Done and release the workspace lock.
        //    The Drop guards will also fire on scope exit — both
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
        self.emit_finished_and_build(app, &job_id)
    }

    /// Run a Review or Test stage end-to-end: invoke the specialist,
    /// parse the Verdict from `assistant_text`, branch on
    /// `approved`. Pulled into a helper because both gates have the
    /// exact same shape (only the prompt + state differ).
    ///
    /// Returns:
    ///
    /// - `Approved(verdict)` — invoke succeeded, parse succeeded,
    ///   `approved=true`. The stage is appended to `Job.stages`
    ///   with `verdict` populated; the FSM should advance.
    /// - `Rejected` — invoke succeeded, parse succeeded,
    ///   `approved=false`. The stage is appended with the verdict;
    ///   the job is finalized as Failed with `last_verdict` set.
    ///   FSM tail should emit `Finished` and return.
    /// - `Terminated` — invoke errored, was cancelled, or the
    ///   verdict was unparseable. Job is already finalized; FSM
    ///   tail should emit `Finished` and return.
    ///
    /// `Result<...>` wraps so SQL failures during finalization
    /// surface as `AppError::Internal` to the IPC caller (the FSM
    /// loop already propagates this via `?`).
    async fn run_verdict_stage<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        state: JobState,
        profile: &Profile,
        prompt: &str,
        job_id: &str,
        workspace_id: &str,
        notify: &Notify,
    ) -> Result<VerdictStageOutcome, AppError> {
        let mut stage = match self
            .run_stage_with_cancel(app, state, profile, prompt, job_id, notify)
            .await
        {
            StageOutcome::Ok(s) => s,
            StageOutcome::Err(e) => {
                self.finalize_failed(
                    job_id,
                    workspace_id,
                    Some(e.to_string()),
                )
                .await?;
                return Ok(VerdictStageOutcome::Terminated);
            }
            StageOutcome::Cancelled => {
                self.finalize_cancelled(job_id, workspace_id).await?;
                return Ok(VerdictStageOutcome::Terminated);
            }
        };

        // Parse the verdict from the assistant text. If parse fails
        // the FSM treats it as a stage failure — `last_error`
        // carries the parse error message; `last_verdict` stays
        // None because we never resolved a structured verdict.
        let verdict = match parse_verdict(&stage.assistant_text) {
            Ok(v) => v,
            Err(e) => {
                self.finalize_failed(
                    job_id,
                    workspace_id,
                    Some(e.to_string()),
                )
                .await?;
                // Even on parse failure we still emit the
                // StageCompleted event so the W3-14 UI sees the raw
                // assistant text; this matches the streaming
                // contract ("the listener observes everything that
                // round-tripped through transport").
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
                return Ok(VerdictStageOutcome::Terminated);
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
            self.finalize_failed_with_verdict(
                job_id,
                workspace_id,
                verdict,
            )
            .await?;
            return Ok(VerdictStageOutcome::Rejected);
        }
        Ok(VerdictStageOutcome::Approved(verdict))
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

/// Three-way outcome of a verdict-gated stage (Review / Test). Used
/// internally by `run_verdict_stage` so the call site can pattern-
/// match on three exhaustive cases without unwrapping nested Result
/// types.
enum VerdictStageOutcome {
    /// Verdict.approved=true. FSM advances to the next stage. The
    /// payload is the parsed Verdict so a future caller could
    /// surface "approved with low-severity issues" even on the
    /// success path; W3-12d itself doesn't read the value.
    Approved(#[allow(dead_code)] Verdict),
    /// Verdict.approved=false. Job is already finalized as Failed
    /// with `last_verdict` populated; FSM tail emits Finished.
    Rejected,
    /// Stage error / cancellation / unparseable verdict. Job is
    /// already finalized; FSM tail emits Finished.
    Terminated,
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
        r.insert(BUILDER_ID.into(), ok_response(build_text, build_cost));
        r.insert(REVIEWER_ID.into(), ok_verdict_response(0.0));
        r.insert(TESTER_ID.into(), ok_verdict_response(0.0));
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

    /// Mock-driven happy path: all five stages OK; reviewer +
    /// tester emit `approved=true` verdicts.
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
        assert_eq!(outcome.stages.len(), 5);
        assert!(outcome.last_error.is_none());
        assert!(outcome.last_verdict.is_none());
        assert!(outcome.total_cost_usd > 0.0);
        assert!(
            (outcome.total_cost_usd - 0.06).abs() < 1e-9,
            "cost sum off (scout+plan+build only since verdicts free): {}",
            outcome.total_cost_usd
        );
        // Stage state ordering matches the FSM's fixed sequence.
        assert_eq!(outcome.stages[0].state, JobState::Scout);
        assert_eq!(outcome.stages[1].state, JobState::Plan);
        assert_eq!(outcome.stages[2].state, JobState::Build);
        assert_eq!(outcome.stages[3].state, JobState::Review);
        assert_eq!(outcome.stages[4].state, JobState::Test);
        // Verdict populated on Review/Test, None on the others.
        assert!(outcome.stages[0].verdict.is_none());
        assert!(outcome.stages[1].verdict.is_none());
        assert!(outcome.stages[2].verdict.is_none());
        assert!(
            outcome.stages[3]
                .verdict
                .as_ref()
                .map(|v| v.approved)
                .unwrap_or(false),
            "review verdict should be approved"
        );
        assert!(
            outcome.stages[4]
                .verdict
                .as_ref()
                .map(|v| v.approved)
                .unwrap_or(false),
            "test verdict should be approved"
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
            .insert(BUILDER_ID.into(), ok_response("unused", 0.0));
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

    /// Planner failure → only scout stage recorded, then Failed.
    #[tokio::test]
    async fn fsm_planner_failure_short_circuits() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses
            .insert(SCOUT_ID.into(), ok_response("scout ok", 0.01));
        responses.insert(PLANNER_ID.into(), err_response("planner boom"));
        responses
            .insert(BUILDER_ID.into(), ok_response("unused", 0.0));
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
        assert_eq!(outcome.stages.len(), 1);
        assert_eq!(outcome.stages[0].state, JobState::Scout);
        assert!(outcome
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("planner boom"));
    }

    /// Builder failure → scout + planner stages recorded, then Failed.
    #[tokio::test]
    async fn fsm_builder_failure_returns_partial_stages() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses
            .insert(SCOUT_ID.into(), ok_response("scout ok", 0.01));
        responses
            .insert(PLANNER_ID.into(), ok_response("plan ok", 0.02));
        responses.insert(BUILDER_ID.into(), err_response("builder boom"));
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
        assert_eq!(outcome.stages.len(), 2);
        assert_eq!(outcome.stages[0].state, JobState::Scout);
        assert_eq!(outcome.stages[1].state, JobState::Plan);
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
        // assertions look at; verdicts come from the helper.
        responses.insert(SCOUT_ID.into(), ok_response("X", 0.0));
        responses.insert(PLANNER_ID.into(), ok_response("Y", 0.0));
        responses.insert(BUILDER_ID.into(), ok_response("Z", 0.0));
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
        responses.insert(BUILDER_ID.into(), ok_response("build", 0.0));
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
        responses.insert(BUILDER_ID.into(), ok_response("b", 0.0));
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
            .find(|(id, _)| id == BUILDER_ID)
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
    #[tokio::test]
    async fn fsm_records_per_stage_duration() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        let sleep = Some(Duration::from_millis(50));
        for (id, sess, text) in [
            (SCOUT_ID, "s1", "scout"),
            (PLANNER_ID, "s2", "plan"),
            (BUILDER_ID, "s3", "build"),
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
            (REVIEWER_ID, "s4"),
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
        assert_eq!(outcome.stages.len(), 5);
        for stage in &outcome.stages {
            assert!(
                stage.duration_ms >= 50,
                "stage {:?} duration_ms = {}",
                stage.state,
                stage.duration_ms
            );
        }
        // Total duration is at least 5*50=250ms.
        assert!(outcome.total_duration_ms >= 250);
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
        // W3-12d: 5 stages (scout / plan / build / review / test).
        assert_eq!(outcome.stages.len(), 5);
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
        // W3-12d: 5 stage rows on the happy path.
        assert_eq!(stage_count, 5, "five stage rows persisted");
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

    /// Pool-backed happy path: the FSM walks all five stages
    /// against a SQLite-backed registry; on completion the
    /// `swarm_jobs` row is `done`, five `swarm_stages` rows, no
    /// `swarm_workspace_locks`.
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
        assert_eq!(outcome.stages.len(), 5);

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
        assert_eq!(stage_count, 5);
        // The Review and Test stage rows carry verdict_json; the
        // others are NULL (Scout/Plan/Build never run a verdict).
        let verdict_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_stages WHERE verdict_json IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(verdict_rows, 2, "review + test stages carry verdict_json");
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
        responses.insert(BUILDER_ID.into(), ok_response("u", 0.0));
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

    /// Pool-backed planner failure: scout stage row persists,
    /// plan/build do not. Final job state is `failed`, lock cleared.
    #[tokio::test]
    async fn fsm_planner_failure_persists_partial_stages_with_pool() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let (registry, pool, _reg_dir) = pool_backed_registry().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), ok_response("scout", 0.01));
        responses.insert(PLANNER_ID.into(), err_response("plan boom"));
        responses.insert(BUILDER_ID.into(), ok_response("u", 0.0));
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
        assert_eq!(outcome.stages.len(), 1);

        let stage_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM swarm_stages")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stage_count, 1, "scout stage row persisted");
        let stage_state: String = sqlx::query_scalar(
            "SELECT state FROM swarm_stages WHERE idx = 0",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stage_state, "scout");
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
        responses.insert(BUILDER_ID.into(), MockResponse {
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
        responses.insert(PLANNER_ID.into(), ok_response("p", 0.0));
        responses.insert(BUILDER_ID.into(), ok_response("b", 0.0));
        responses.insert(
            REVIEWER_ID.into(),
            rejected_verdict_response("unwrap on None"),
        );
        // Tester response is never consumed (Review rejection
        // short-circuits the chain) but the mock requires entries
        // to exist for any id the FSM might ask about; the FSM is
        // careful to NOT call tester on the rejected path.
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
        // Stages: scout/plan/build (3) + review (1) — tester didn't run.
        assert_eq!(outcome.stages.len(), 4);
        assert_eq!(outcome.stages[3].state, JobState::Review);
        assert!(outcome.stages[3].verdict.is_some());
        assert_eq!(
            outcome.stages[3].verdict.as_ref().map(|v| v.approved),
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

    /// Tester returns `approved=false` → job finalizes Failed at
    /// the Test gate with `last_verdict` populated.
    #[tokio::test]
    async fn fsm_test_rejection_finalizes_failed_with_verdict() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), ok_response("s", 0.0));
        responses.insert(PLANNER_ID.into(), ok_response("p", 0.0));
        responses.insert(BUILDER_ID.into(), ok_response("b", 0.0));
        responses.insert(REVIEWER_ID.into(), ok_verdict_response(0.0));
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
        // Stages: scout/plan/build (3) + review (approved) + test (rejected) = 5.
        assert_eq!(outcome.stages.len(), 5);
        assert_eq!(outcome.stages[4].state, JobState::Test);
        let test_verdict =
            outcome.stages[4].verdict.as_ref().expect("test verdict");
        assert!(!test_verdict.approved);
        assert!(outcome.last_error.is_none());
        let lv = outcome.last_verdict.as_ref().expect("last_verdict set");
        assert_eq!(lv.issues[0].message, "test_foo failed");
    }

    /// Reviewer's assistant_text is unparseable (not JSON, not
    /// fenced, not a balanced object). FSM finalizes Failed with
    /// `last_error` mentioning "could not parse Verdict".
    /// `last_verdict` stays None — there's no structured verdict.
    #[tokio::test]
    async fn fsm_review_unparseable_finalizes_failed() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), ok_response("s", 0.0));
        responses.insert(PLANNER_ID.into(), ok_response("p", 0.0));
        responses.insert(BUILDER_ID.into(), ok_response("b", 0.0));
        responses.insert(
            REVIEWER_ID.into(),
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
        // UI sees the raw assistant_text). Stage count = 4.
        assert_eq!(outcome.stages.len(), 4);
        assert_eq!(outcome.stages[3].state, JobState::Review);
        // No structured verdict — parse failed.
        assert!(outcome.stages[3].verdict.is_none());
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
    /// flow.
    #[tokio::test]
    async fn fsm_review_skipped_when_build_fails() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let mut responses: HashMap<String, MockResponse> = HashMap::new();
        responses.insert(SCOUT_ID.into(), ok_response("s", 0.0));
        responses.insert(PLANNER_ID.into(), ok_response("p", 0.0));
        responses.insert(BUILDER_ID.into(), err_response("builder boom"));
        // These are never consumed since Builder errors first.
        responses.insert(REVIEWER_ID.into(), ok_verdict_response(0.0));
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
        // Scout + Plan succeeded; Build erred; Review/Test never run.
        assert_eq!(outcome.stages.len(), 2);
        for stage in &outcome.stages {
            assert!(matches!(
                stage.state,
                JobState::Scout | JobState::Plan
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
        assert_eq!(outcome.stages.len(), 5);
    }

    /// Sanity: the registry indeed has the five FSM specialist
    /// ids — guards against future renames in the bundled `.md`
    /// files breaking the FSM contract silently.
    #[test]
    fn bundled_registry_has_five_specialist_ids() {
        let reg = bundled_registry();
        for id in [SCOUT_ID, PLANNER_ID, BUILDER_ID, REVIEWER_ID, TESTER_ID] {
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
    /// W3-12d: the set now covers all five stages — reviewer +
    /// tester emit `approved=true` verdicts so the FSM reaches
    /// Done.
    fn happy_responses() -> HashMap<String, MockResponse> {
        let mut r: HashMap<String, MockResponse> = HashMap::new();
        let sleep = Some(Duration::from_millis(100));
        let stages: &[(&str, &str, &str, f64)] = &[
            (SCOUT_ID, "sess-scout", "scout findings", 0.01),
            (PLANNER_ID, "sess-plan", "plan steps", 0.02),
            (BUILDER_ID, "sess-build", "build done", 0.03),
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
            (REVIEWER_ID, "sess-review"),
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
        // Exact ordered stream on the happy path: 5 (started, 5x
        // (stage_started, stage_completed), finished).
        assert_eq!(
            kinds,
            vec![
                KIND_STARTED,
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                KIND_STAGE_STARTED,
                KIND_STAGE_COMPLETED,
                KIND_FINISHED,
            ],
            "full ordered stream mismatch; kinds={kinds:?}"
        );

        // Each StageStarted carries the upcoming state; assert the
        // canonical `scout → plan → build → review → test` order.
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
            vec!["scout", "plan", "build", "review", "test"]
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
        assert_eq!(stages.len(), 5);
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
        responses.insert(PLANNER_ID.into(), MockResponse {
            result: Err(AppError::SwarmInvoke("planner boom".into())),
            sleep: Some(Duration::from_millis(100)),
        });
        responses.insert(BUILDER_ID.into(), ok_response("unused", 0.0));
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
        responses.insert(PLANNER_ID.into(), MockResponse {
            result: Ok(InvokeResult {
                session_id: "p".into(),
                assistant_text: "plan".into(),
                total_cost_usd: 0.01,
                turn_count: 1,
            }),
            sleep: plan_sleep,
        });
        responses.insert(BUILDER_ID.into(), MockResponse {
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
}
