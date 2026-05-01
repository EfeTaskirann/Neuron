//! `secrets:*` namespace — OS keychain CRUD (WP-W3-01).
//!
//! - `secrets:set(key, value)` → `()` — empty `value` rejected
//!   with `AppError::InvalidInput` (use `delete` for absence).
//! - `secrets:has(key)` → `bool` — cheap presence probe used by
//!   the Settings route's "Configure API keys" CTA. **Never**
//!   returns the value.
//! - `secrets:delete(key)` → `()` — idempotent; missing entry is
//!   not an error.
//!
//! ## Why no `secrets:get` command
//!
//! The frontend has no business reading secret values back across
//! the IPC boundary. CTAs only need `secrets:has` plus the
//! structured `AppError::NoApiKey` surfaced by the *consumers*
//! (`mcp:install`, `runs:create`) when a secret is missing. Adding
//! `secrets:get` would create an exfiltration vector — a
//! compromised renderer could read any secret without going
//! through the keyring access prompt the OS shows. The Charter's
//! hard constraint #2 ("API keys live in OS keychain — never
//! plaintext") combined with WP-W3-09 (capabilities tightening)
//! treats this as a forbidden command shape.
//!
//! ## IPC naming
//!
//! Same convention as the rest of the command surface: snake_case
//! Rust function name on the IPC side, camelCase TS façade
//! (`commands.secretsSet/Has/Delete`) emitted by `tauri-specta`.

use crate::error::AppError;
use crate::secrets;

/// Write a secret to the OS keychain. Empty `value` is rejected so
/// the keychain never holds a sentinel "" that `has_secret` would
/// then have to second-guess.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn secrets_set(key: String, value: String) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return Err(AppError::InvalidInput("key must not be empty".into()));
    }
    if value.is_empty() {
        return Err(AppError::InvalidInput(
            "value must not be empty (use secrets:delete to clear)".into(),
        ));
    }
    secrets::set_secret(&key, &value)
}

/// Cheap presence probe. Honors the env-override path so
/// `NEURON_<KEY>=…` exported into a developer's shell counts as
/// "configured" without writing to the keychain first.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn secrets_has(key: String) -> Result<bool, AppError> {
    if key.trim().is_empty() {
        return Err(AppError::InvalidInput("key must not be empty".into()));
    }
    secrets::has_secret(&key)
}

/// Delete the keychain entry. Idempotent — a missing entry is not
/// an error so the frontend's "Forget API key" CTA can be wired
/// without a guard.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn secrets_delete(key: String) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return Err(AppError::InvalidInput("key must not be empty".into()));
    }
    secrets::delete_secret(&key)
}

#[cfg(test)]
mod tests {
    //! Tests use unique env-override keys (so set_var/remove_var
    //! cannot collide with sibling tests on the shared thread
    //! pool) and never touch the OS keychain — the
    //! `secrets::tests::keychain_round_trip` integration test
    //! covers that path under `--ignored`.
    use super::*;

    #[tokio::test]
    async fn secrets_set_rejects_empty_value() {
        let err = secrets_set("anthropic".into(), "".into())
            .await
            .expect_err("empty value rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    #[tokio::test]
    async fn secrets_set_rejects_empty_key() {
        let err = secrets_set("".into(), "v".into())
            .await
            .expect_err("empty key rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    #[tokio::test]
    async fn secrets_has_rejects_empty_key() {
        let err = secrets_has("".into())
            .await
            .expect_err("empty key rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    #[tokio::test]
    async fn secrets_delete_rejects_empty_key() {
        let err = secrets_delete("".into())
            .await
            .expect_err("empty key rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Round-trip via the env-override path. The presence probe
    /// must reflect a freshly-exported `NEURON_<KEY>` without any
    /// keychain write.
    #[tokio::test]
    async fn secrets_has_via_env_override_round_trip() {
        let key = "wp_w3_01_cmd_has_round_trip";
        let env = "NEURON_WP_W3_01_CMD_HAS_ROUND_TRIP";
        std::env::set_var(env, "secret-value");
        let got = secrets_has(key.into()).await;
        std::env::remove_var(env);
        assert!(matches!(got, Ok(true)));
    }

    /// `secrets:has` returns false (or — on backend-less CI — an
    /// internal error) for an unconfigured key. Either path proves
    /// the value is not exposed.
    #[tokio::test]
    async fn secrets_has_false_when_unconfigured() {
        let key = "wp_w3_01_cmd_definitely_unset";
        // Belt-and-suspenders: clear any inherited env var.
        std::env::remove_var("NEURON_WP_W3_01_CMD_DEFINITELY_UNSET");
        match secrets_has(key.into()).await {
            Ok(false) => {}
            Err(AppError::Internal(_)) => {} // CI fallback
            other => panic!("unexpected result: {other:?}"),
        }
    }
}
