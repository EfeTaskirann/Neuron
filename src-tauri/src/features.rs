//! Developer features, opt in with `NEURON_DEV_FEATURES=1` at launch.
//!
//! Neuron's flagship is the Terminal Swarm. The older surfaces stay in
//! the codebase but out of the way unless asked for:
//!
//!   * the Workflow canvas and its LangGraph Python sidecar (the sidecar
//!     is not started),
//!   * Runs and Agents,
//!   * the headless swarm (`swarm::`), which runs `claude` with
//!     `--dangerously-skip-permissions` — its commands refuse to run.
//!
//! The flag is read once, at startup, into the managed [`DevFeatures`]
//! state; the frontend reads it through
//! [`crate::commands::features::app_features`] to show their routes.

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;

/// Environment variable that turns developer features on.
pub const DEV_FEATURES_ENV: &str = "NEURON_DEV_FEATURES";

/// Managed app state: whether developer features are on for this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevFeatures(pub bool);

impl DevFeatures {
    /// From [`DEV_FEATURES_ENV`]: `1`, `true`, `yes` or `on`, any case.
    pub fn from_env() -> Self {
        Self(flag_on(std::env::var(DEV_FEATURES_ENV).ok().as_deref()))
    }
}

fn flag_on(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Whether developer features are on. An app that never managed
/// [`DevFeatures`] has them off.
pub fn dev_features_on<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<DevFeatures>().is_some_and(|f| f.0)
}

/// Refuses a developer-only command (`what` names it) unless developer
/// features are on.
pub fn require_dev_features<R: Runtime>(app: &AppHandle<R>, what: &str) -> Result<(), AppError> {
    if dev_features_on(app) {
        Ok(())
    } else {
        Err(AppError::FeatureDisabled(format!(
            "{what} bir geliştirici özelliği: Neuron'u {DEV_FEATURES_ENV}=1 ile başlat."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(features: Option<bool>) -> tauri::App<tauri::test::MockRuntime> {
        let builder = tauri::test::mock_builder();
        let builder = match features {
            Some(on) => builder.manage(DevFeatures(on)),
            None => builder,
        };
        builder
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app")
    }

    #[test]
    fn only_explicit_yes_values_turn_the_flag_on() {
        for on in ["1", "true", "TRUE", " yes ", "On"] {
            assert!(flag_on(Some(on)), "{on:?}");
        }
        for off in ["", "0", "false", "no", "off", "2", "enabled"] {
            assert!(!flag_on(Some(off)), "{off:?}");
        }
        assert!(!flag_on(None));
    }

    #[test]
    fn features_are_off_unless_managed_on() {
        assert!(!dev_features_on(app(None).handle()));
        assert!(!dev_features_on(app(Some(false)).handle()));
        assert!(dev_features_on(app(Some(true)).handle()));
    }

    #[test]
    fn a_developer_command_is_refused_with_the_way_to_enable_it() {
        let off = app(None);
        match require_dev_features(off.handle(), "Headless swarm") {
            Err(AppError::FeatureDisabled(m)) => {
                assert!(m.contains("Headless swarm"), "{m}");
                assert!(m.contains("NEURON_DEV_FEATURES=1"), "{m}");
            }
            other => panic!("not refused: {other:?}"),
        }
        assert!(require_dev_features(app(Some(true)).handle(), "x").is_ok());
    }
}
