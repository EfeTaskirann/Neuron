//! `subscription_env()` reads `std::env::vars()` at call time, so
//! the strip-tests mutate process-global env. Each test owns its
//! own variable name (`NEURON_TEST_SE_*`) to avoid races with
//! other tests in the suite, then restores the prior value at end.

use super::{build_specialist_args, subscription_env, STRIPPED_ENV_VARS};
use crate::swarm::profile::{PermissionMode, Profile};
use std::path::{Path, PathBuf};

fn fixture_profile(mode: PermissionMode) -> Profile {
    Profile {
        id: "test".into(),
        version: "1.0.0".into(),
        role: "Test".into(),
        description: "Test".into(),
        allowed_tools: vec!["Read".into(), "Grep".into()],
        permission_mode: mode,
        max_turns: 7,
        body: "persona body".into(),
        source_path: PathBuf::from("test.md"),
    }
}

/// Save+set+restore an env var for the duration of a closure.
/// Process-global, so callers picking unique names are responsible
/// for thread safety of *that* variable.
fn with_env<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
    let prior = std::env::var(key).ok();
    match value {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    f();
    match prior {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

#[test]
fn subscription_env_strips_api_key() {
    with_env("ANTHROPIC_API_KEY", Some("sk-test-fake"), || {
        let env = subscription_env();
        assert!(
            !env.contains_key("ANTHROPIC_API_KEY"),
            "ANTHROPIC_API_KEY must not survive subscription_env()"
        );
    });
}

#[test]
fn subscription_env_strips_provider_routes() {
    with_env("USE_BEDROCK", Some("1"), || {
        with_env("USE_VERTEX", Some("1"), || {
            with_env("USE_FOUNDRY", Some("1"), || {
                let env = subscription_env();
                for var in ["USE_BEDROCK", "USE_VERTEX", "USE_FOUNDRY"] {
                    assert!(
                        !env.contains_key(var),
                        "{var} must not survive subscription_env()"
                    );
                }
            });
        });
    });
}

/// 2026-05-13 `/login` regression: a parent `claude` shell exports
/// `CLAUDE_CODE_OAUTH_TOKEN` for nested processes so they can
/// authenticate against the same session. When a SUB-claude
/// spawned by Neuron inherits this token, it prefers it over the
/// per-pane `.credentials.json` we seeded — and if the session
/// scope differs, the spawned claude drops into `/login`. Strip
/// it (and the related auth-bypass vars) so every spawn uses the
/// credentials we control.
#[test]
fn subscription_env_strips_oauth_token_and_endpoint_overrides() {
    let auth_vars = [
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_API_URL",
        "ANTHROPIC_CUSTOM_HEADERS",
        "CLAUDE_CONFIG_DIR",
    ];
    // Set each one in turn and verify subscription_env() drops
    // it. We don't set all six at once because `with_env` doesn't
    // compose more than ~3 deep without becoming illegible —
    // serial per-var checks give the same coverage.
    for var in auth_vars {
        with_env(var, Some("set-by-parent-shell"), || {
            let env = subscription_env();
            assert!(
                !env.contains_key(var),
                "{var} must not survive subscription_env() — \
                 parent-supplied auth/config would override the \
                 per-pane credentials seed"
            );
        });
    }
}

/// The canonical strip list must include the OAuth bypass set so
/// the brain spawn paths (`transport`, `persistent_session`) — which
/// iterate over `STRIPPED_ENV_VARS` directly when calling
/// `Command::env_remove` — also clear it from the inherited env.
#[test]
fn stripped_env_vars_includes_oauth_bypass_set() {
    for required in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CONFIG_DIR",
    ] {
        assert!(
            STRIPPED_ENV_VARS.contains(&required),
            "STRIPPED_ENV_VARS must list {required} — both spawn \
             paths read this constant to decide which env vars \
             to remove from the child"
        );
    }
}

/// Pass-through env vars survive (negative control for the strip).
#[test]
fn subscription_env_preserves_other_vars() {
    let key = "NEURON_TEST_SE_PASSTHROUGH";
    with_env(key, Some("hello"), || {
        let env = subscription_env();
        assert_eq!(env.get(key).map(String::as_str), Some("hello"));
    });
}

/// WP §7 — argv carries the required flags and never the replace-
/// mode system-prompt flags.
#[test]
fn specialist_args_contain_required_flags() {
    let profile = fixture_profile(PermissionMode::AcceptEdits);
    let args = build_specialist_args(
        &profile,
        Path::new("/tmp/sys-prompt.md"),
    );
    let joined = args.join(" ");
    assert!(joined.contains("-p"));
    assert!(joined.contains("--input-format stream-json"));
    assert!(joined.contains("--output-format stream-json"));
    assert!(joined.contains("--append-system-prompt-file"));
    assert!(joined.contains("--max-turns 7"));
    assert!(joined.contains("--allowedTools Read,Grep"));
    // Replace-mode flags must NEVER appear.
    assert!(
        !args.iter().any(|a| a == "--system-prompt"),
        "--system-prompt is forbidden"
    );
    assert!(
        !args.iter().any(|a| a == "--system-prompt-file"),
        "--system-prompt-file (replace mode) is forbidden"
    );
}

/// WP §7 — Plan mode emits `--permission-mode plan` and skips the
/// dangerous flag.
#[test]
fn plan_mode_skips_dangerous_flag() {
    let profile = fixture_profile(PermissionMode::Plan);
    let args = build_specialist_args(
        &profile,
        Path::new("/tmp/sys-prompt.md"),
    );
    assert!(args.iter().any(|a| a == "--permission-mode"));
    assert!(args.iter().any(|a| a == "plan"));
    assert!(
        !args.iter().any(|a| a == "--dangerously-skip-permissions"),
        "Plan mode must NOT include --dangerously-skip-permissions"
    );
}

/// AcceptAll mirrors AcceptEdits in Phase 1 (binary gate).
#[test]
fn accept_all_mode_emits_dangerous_flag() {
    let profile = fixture_profile(PermissionMode::AcceptAll);
    let args = build_specialist_args(
        &profile,
        Path::new("/tmp/sys-prompt.md"),
    );
    assert!(args.iter().any(|a| a == "--dangerously-skip-permissions"));
    assert!(
        !args.iter().any(|a| a == "--permission-mode"),
        "non-plan modes must NOT carry --permission-mode"
    );
}
