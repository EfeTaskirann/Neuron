//! argv builder for a one-shot per-invoke specialist `claude` call. See
//! the module-level docs (`super`) for the responsibility split; this
//! file owns responsibility #3 (argv construction).

use std::path::Path;

use crate::swarm::profile::{PermissionMode, Profile};

/// Build the argv for a one-shot per-invoke specialist call. The flag
/// order is the contract from WP §3:
///
/// ```text
/// -p
/// --input-format stream-json
/// --output-format stream-json
/// --verbose
/// --append-system-prompt-file <system_prompt_file>
/// --max-turns <profile.max_turns>
/// (--permission-mode plan)            -- only when permission_mode == Plan
/// (--dangerously-skip-permissions)    -- only when permission_mode != Plan
/// --allowedTools "<comma-joined profile.allowed_tools>"
/// ```
///
/// `--system-prompt` and `--system-prompt-file` (replace mode) are
/// **never** emitted — only `--append-system-prompt-file`. Replacing
/// the system prompt would drop Claude Code's default tool-use
/// conditioning, which the persona depends on (WP §"Hard rules").
pub fn build_specialist_args(
    profile: &Profile,
    system_prompt_file: &Path,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--append-system-prompt-file".into(),
        system_prompt_file.to_string_lossy().into_owned(),
        "--max-turns".into(),
        profile.max_turns.to_string(),
    ];

    match profile.permission_mode {
        PermissionMode::Plan => {
            // Plan mode: explicit `--permission-mode plan`; the
            // dangerous-skip flag is omitted so prompted approvals
            // still surface. The Coordinator (W3-12) will catch them.
            args.push("--permission-mode".into());
            args.push("plan".into());
        }
        PermissionMode::AcceptEdits | PermissionMode::AcceptAll => {
            // Phase 1 binary gate per WP §3 "Permissions note":
            // anything past `Plan` skips prompts wholesale so the
            // smoke command can run without a UI. W3-12 splits these
            // into per-tool allow / deny lists.
            args.push("--dangerously-skip-permissions".into());
        }
    }

    args.push("--allowedTools".into());
    args.push(profile.allowed_tools.join(","));
    args
}
