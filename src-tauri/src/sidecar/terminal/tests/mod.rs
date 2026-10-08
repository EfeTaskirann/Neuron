//! Unit coverage focuses on the deterministic helpers (ring buffer
//! overflow, CSI stripper, agent inference, regex set, default
//! shell resolution). One opt-in (`#[ignore]`d) integration test
//! spawns a real shell to exercise the full pipeline; it stays
//! out of the default suite because CI runners on minimal images
//! may not have a usable shell on PATH.
//!
//! Module layout mirrors the production submodules under
//! `sidecar/terminal/`:
//!   - `text`     — CSI stripper / DSR-CPR extraction / UTF-8 line helpers
//!   - `command`  — tokenizer, default-shell, agent-kind inference, cwd expand
//!   - `approval` — per-agent awaiting-approval regex set
//!   - `registry` — ring-buffer overflow + `pane_lines` DB read-back +
//!     the two opt-in real-shell integration smokes
mod approval;
mod command;
mod registry;
mod text;
