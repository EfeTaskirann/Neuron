//! Spawn entry point — called from `lib.rs::run().setup(...)`.
//!
//! Constructs the `python -m agent_runtime` child, installs the stdout
//! read loop (`super::reader::read_loop`), and hands back a
//! `SidecarHandle` for app state. `resolve_python` is the interpreter /
//! working-directory search used at spawn time.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Manager, Runtime};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::db::DbPool;
use crate::error::AppError;

use super::reader::read_loop;
use super::{Inner, SidecarHandle};

/// Spawn `python -m agent_runtime` as a managed child process, install
/// a stdout-reading task that converts wire events into DB writes +
/// Tauri events, and return a handle the runtime can stash in app
/// state.
///
/// The subprocess runs from `src-tauri/sidecar/agent_runtime/` so the
/// `agent_runtime` package is importable and the `.venv` Python
/// interpreter is on disk relative to the manifest.
pub fn spawn_runtime<R: Runtime>(app: &AppHandle<R>) -> Result<SidecarHandle, AppError> {
    let app_for_loop = app.clone();
    let pool = app
        .try_state::<DbPool>()
        .ok_or_else(|| AppError::Sidecar("DbPool not in app state — call db::init first".into()))?
        .inner()
        .clone();

    let (python, working_dir) = resolve_python()?;

    // `kill_on_drop` is the seatbelt for the case where the Tauri
    // builder panics after we spawned the child but before the
    // setup hook returns — `Child::drop` then sends SIGKILL / TerminateProcess.
    //
    // `PYTHONUNBUFFERED=1` forces stdout to flush per-write; without it
    // Python block-buffers when stdout is a pipe (~4–8 KiB) so small
    // span frames sit in the buffer until enough volume accumulates,
    // and the supervisor's read loop appears stalled to the UI.
    let mut child = Command::new(python)
        .arg("-m")
        .arg("agent_runtime")
        .current_dir(&working_dir)
        .env("PYTHONUNBUFFERED", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        // Inherit stderr so Python tracebacks land in the dev console.
        // `Stdio::piped()` would force us to consume them or risk a
        // pipe-full deadlock; stderr inheritance is the simplest
        // correct path for Week 2.
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            AppError::Sidecar(format!(
                "failed to spawn LangGraph sidecar (working dir: {}): {e}",
                working_dir.display()
            ))
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::Sidecar("child stdin pipe missing".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Sidecar("child stdout pipe missing".into()))?;

    // Spawn the read loop on Tauri's tokio runtime. The loop ends
    // naturally on stdout EOF (child exited) or on a hard frame error.
    // Hold the JoinHandle so `SidecarHandle::shutdown` can `await` it
    // (with a bounded grace) before killing the child.
    let read_loop_handle =
        tauri::async_runtime::spawn(read_loop(stdout, pool, app_for_loop));

    let inner = Arc::new(Inner {
        stdin: Mutex::new(Some(stdin)),
        child: Mutex::new(Some(child)),
        read_loop: Mutex::new(Some(read_loop_handle)),
    });

    Ok(SidecarHandle::from_inner(inner))
}

/// Resolve the Python interpreter and the working directory for the
/// sidecar process.
///
/// Search order (first hit wins):
///
/// 1. `NEURON_AGENT_PYTHON` env var — explicit override (CI / tests).
/// 2. The uv-managed venv at `<sidecar>/.venv/` (`Scripts/python.exe`
///    on Windows, `bin/python` elsewhere).
/// 3. Bare `python` on PATH (developer dev shell).
fn resolve_python() -> Result<(PathBuf, PathBuf), AppError> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let working_dir = manifest_dir.join("sidecar").join("agent_runtime");

    if let Ok(p) = std::env::var("NEURON_AGENT_PYTHON") {
        return Ok((PathBuf::from(p), working_dir));
    }

    let venv_python = if cfg!(windows) {
        working_dir.join(".venv").join("Scripts").join("python.exe")
    } else {
        working_dir.join(".venv").join("bin").join("python")
    };

    if venv_python.is_file() {
        return Ok((venv_python, working_dir));
    }

    Ok((PathBuf::from("python"), working_dir))
}
