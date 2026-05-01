//! WP-W3-01 — OS keychain bridge.
//!
//! Charter §"Hard constraints" #2: "API keys live in OS keychain —
//! never plaintext, never `.env` committed". Every secret read from
//! Rust (provider tokens for the agent runtime, MCP server tokens
//! like `GITHUB_PERSONAL_ACCESS_TOKEN`, future webhook signing keys)
//! flows through this module rather than touching `std::env::var`
//! directly.
//!
//! ## Resolution order
//!
//! Mirrors `agent_runtime/secrets.py::get_provider_key` so the Rust
//! and Python sides stay observably-equivalent:
//!
//! 1. **Test/dev env override `NEURON_<KEY>`** — uppercased, with
//!    non-alphanumeric characters replaced by `_`. Used by
//!    `cargo test` and developer escape hatches; never advertised
//!    in user docs. An empty value is treated as absent (matches
//!    the `Ok(v) if !v.is_empty()` guard previously inlined in
//!    `mcp::registry::resolve_env`).
//! 2. **OS keychain** via `keyring::Entry::new("neuron", key)?
//!    .get_password()`. The service name `"neuron"` is the same
//!    string the Python sidecar uses (`SERVICE = "neuron"` in
//!    `agent_runtime/secrets.py`), so one key written via
//!    `secrets:set('anthropic', …)` is readable by both runtimes.
//!
//! ## Error mapping
//!
//! - `keyring::Error::NoEntry` is **not** an error: `get_secret`
//!   returns `Ok(None)`, `has_secret` returns `Ok(false)`.
//! - Every other `keyring::Error` widens to `AppError::Internal`.
//!   The error message **never** includes the secret value (which
//!   the keyring crate would never expose anyway, but the rule is
//!   documented here so future contributors don't add it back).
//!
//! ## Why not call `keyring` from `commands/secrets.rs` directly
//!
//! Keeping the keyring boundary in one module:
//! - lets `mcp::registry::resolve_env` reuse the same env-override
//!   path without duplicating the precedence rules (the existing
//!   `requires_secret: "GITHUB_PERSONAL_ACCESS_TOKEN"` flow keeps
//!   working from a developer's shell);
//! - gives unit tests a single seam (`#[cfg(test)] mod tests`) to
//!   exercise the resolution chain via env-override without
//!   touching the OS keychain — CI runners famously do not have
//!   one.

use crate::error::AppError;

/// Fixed service-name constant used for every keychain entry.
///
/// **DO NOT** change without coordinating with the Python sidecar
/// (`agent_runtime/secrets.py:SERVICE`). The two strings must stay
/// byte-identical or the keyring lookup hits a different namespace
/// on each side.
const SERVICE: &str = "neuron";

/// Compute the env-var name for a given secret key.
///
/// `"anthropic"` → `"NEURON_ANTHROPIC"`,
/// `"github-pat"` → `"NEURON_GITHUB_PAT"`,
/// `"GITHUB_PERSONAL_ACCESS_TOKEN"` → `"NEURON_GITHUB_PERSONAL_ACCESS_TOKEN"`.
///
/// We uppercase and replace any non-alphanumeric byte with `_` so
/// the resulting name is always a valid POSIX environment variable
/// identifier even if the secret key contains hyphens or dots
/// (future `webhook.signing` style keys).
fn env_override_name(key: &str) -> String {
    let mut s = String::with_capacity(7 + key.len());
    s.push_str("NEURON_");
    for ch in key.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch.to_ascii_uppercase());
        } else {
            s.push('_');
        }
    }
    s
}

/// Read the env-var override for `key`, returning `Some(value)`
/// only if the variable is set and non-empty. An empty value is
/// treated as absent on purpose — matches the historical
/// `Ok(v) if !v.is_empty()` guard in `mcp::registry::resolve_env`
/// and means an exporter accidentally clearing `NEURON_*` does not
/// silently win over a real keychain entry.
fn env_override(key: &str) -> Option<String> {
    let name = env_override_name(key);
    match std::env::var(&name) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// Look up a secret. Returns `Ok(None)` when the secret is not
/// configured (env override absent AND keychain has no entry).
pub fn get_secret(key: &str) -> Result<Option<String>, AppError> {
    if let Some(v) = env_override(key) {
        return Ok(Some(v));
    }
    let entry = keyring::Entry::new(SERVICE, key)
        .map_err(|e| AppError::Internal(format!("keyring entry: {e}")))?;
    match entry.get_password() {
        Ok(v) if v.is_empty() => Ok(None),
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AppError::Internal(format!("keyring read: {e}"))),
    }
}

/// Write a secret to the keychain. Empty values are rejected
/// upstream by `commands::secrets::secrets_set`; this layer trusts
/// its caller and writes whatever it's given.
pub fn set_secret(key: &str, value: &str) -> Result<(), AppError> {
    let entry = keyring::Entry::new(SERVICE, key)
        .map_err(|e| AppError::Internal(format!("keyring entry: {e}")))?;
    entry
        .set_password(value)
        .map_err(|e| AppError::Internal(format!("keyring write: {e}")))?;
    Ok(())
}

/// Cheap presence check. The env override path also counts — a
/// developer with `NEURON_ANTHROPIC=...` exported into their shell
/// gets `has_secret("anthropic") == true` without writing to the
/// keychain.
pub fn has_secret(key: &str) -> Result<bool, AppError> {
    Ok(get_secret(key)?.is_some())
}

/// Delete the keychain entry for `key`. **Idempotent**: a missing
/// entry returns `Ok(())` rather than erroring, so the frontend's
/// "Forget API key" CTA can be wired without a guard.
///
/// The env override is **not** cleared (we don't mutate the
/// process environment) — callers should rely on the convention
/// that `NEURON_*` is only set by tests and dev shells.
pub fn delete_secret(key: &str) -> Result<(), AppError> {
    let entry = keyring::Entry::new(SERVICE, key)
        .map_err(|e| AppError::Internal(format!("keyring entry: {e}")))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AppError::Internal(format!("keyring delete: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests cover the env-override path exclusively. The
    //! keychain-backed branch lives behind `#[ignore]` because CI
    //! runners do not provision a credential store and the tests
    //! would otherwise fail with `keyring::Error::NoStorageAccess`
    //! on a freshly-spun container.
    //!
    //! `std::env::set_var` is process-global, so each test uses a
    //! **unique** key whose `NEURON_*` name cannot collide with a
    //! sibling test running on the same thread pool. Avoids the
    //! `serial_test` dep churn the WP body warned about.
    use super::*;

    /// `env_override_name` must produce a valid POSIX env-var
    /// identifier regardless of separator characters in the key.
    #[test]
    fn env_override_name_uppercases_and_substitutes_nonalnum() {
        assert_eq!(env_override_name("anthropic"), "NEURON_ANTHROPIC");
        assert_eq!(env_override_name("github-pat"), "NEURON_GITHUB_PAT");
        assert_eq!(
            env_override_name("webhook.signing"),
            "NEURON_WEBHOOK_SIGNING"
        );
        assert_eq!(
            env_override_name("GITHUB_PERSONAL_ACCESS_TOKEN"),
            "NEURON_GITHUB_PERSONAL_ACCESS_TOKEN"
        );
    }

    /// Acceptance: an exported `NEURON_<KEY>` short-circuits the
    /// keychain lookup. Uses a unique key so it cannot clash with
    /// any concurrent test.
    #[test]
    fn env_override_short_circuits_keychain() {
        let key = "wp_w3_01_env_override_smoke";
        let env = "NEURON_WP_W3_01_ENV_OVERRIDE_SMOKE";
        // SAFETY: setting an env var is unsafe in Rust 2024+ but
        // allowed here on edition 2021 — the workspace pins to it.
        std::env::set_var(env, "shhh");
        let got = get_secret(key).expect("ok");
        std::env::remove_var(env);
        assert_eq!(got.as_deref(), Some("shhh"));
    }

    /// Empty env-override behaves as "absent" — falls through to
    /// the keychain branch which (without a real keychain entry)
    /// returns None. Defends the historical non-empty guard.
    #[test]
    fn empty_env_override_treated_as_absent() {
        let key = "wp_w3_01_empty_env_override";
        let env = "NEURON_WP_W3_01_EMPTY_ENV_OVERRIDE";
        std::env::set_var(env, "");
        let got = get_secret(key);
        std::env::remove_var(env);
        // The keychain branch may either return Ok(None) (no
        // credential exists) or Err(Internal) (no backend on the
        // CI runner). Both are valid "absent" signals — what we
        // assert is the empty env override did NOT win.
        match got {
            Ok(v) => assert!(v.is_none(), "empty override must not produce Some"),
            Err(_) => {} // CI without keychain — acceptable
        }
    }

    /// `has_secret` returns true via the env-override path even
    /// when no keychain entry exists. The frontend uses this to
    /// gate the "Configure API keys" CTA — a developer exporting
    /// `NEURON_OPENAI=...` should see the CTA disappear.
    #[test]
    fn has_secret_true_via_env_override() {
        let key = "wp_w3_01_has_via_override";
        let env = "NEURON_WP_W3_01_HAS_VIA_OVERRIDE";
        std::env::set_var(env, "yes");
        let got = has_secret(key);
        std::env::remove_var(env);
        assert!(matches!(got, Ok(true)));
    }

    /// Unset `NEURON_*` + no keychain entry → `Ok(None)`.
    /// Distinguishes "no entry" from "backend error". On CI without
    /// any keychain backend the call may return an Internal error;
    /// we accept both as "unconfigured" since neither path returns
    /// a value.
    #[test]
    fn missing_secret_returns_none_or_backend_error() {
        let key = "wp_w3_01_definitely_unset";
        let env = "NEURON_WP_W3_01_DEFINITELY_UNSET";
        std::env::remove_var(env);
        match get_secret(key) {
            Ok(v) => assert!(v.is_none()),
            Err(AppError::Internal(_)) => {} // CI fallback
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }

    /// Round-trip via the OS keychain. `#[ignore]` because GitHub
    /// Actions / vanilla CI containers do not run a credential
    /// store. Run locally with:
    ///
    /// ```text
    /// cargo test --manifest-path src-tauri/Cargo.toml \
    ///   --lib secrets::tests::keychain_round_trip -- --ignored
    /// ```
    #[test]
    #[ignore = "requires OS keychain — opt in via --ignored"]
    fn keychain_round_trip() {
        let key = "wp_w3_01_keychain_round_trip";
        // Make absolutely sure the env override is not in play.
        std::env::remove_var(env_override_name(key));

        set_secret(key, "deadbeef").expect("set_secret");
        let got = get_secret(key).expect("get_secret").expect("Some");
        assert_eq!(got, "deadbeef");
        assert!(has_secret(key).expect("has_secret"));

        delete_secret(key).expect("delete_secret");
        // delete is idempotent
        delete_secret(key).expect("delete_secret idempotent");
        assert!(!has_secret(key).expect("has after delete"));
    }
}
