//! `app` command namespace — what this run of the app offers.

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Runtime};

use crate::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AppFeatures {
    /// Developer features are on (`NEURON_DEV_FEATURES=1` at launch):
    /// the Workflow canvas, Runs, Agents and the headless Swarm.
    pub dev_features: bool,
}

#[tauri::command]
#[specta::specta]
pub async fn app_features<R: Runtime>(app: AppHandle<R>) -> Result<AppFeatures, AppError> {
    Ok(AppFeatures {
        dev_features: crate::features::dev_features_on(&app),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::DevFeatures;

    #[tokio::test]
    async fn reports_the_managed_flag() {
        for on in [false, true] {
            let app = tauri::test::mock_builder()
                .manage(DevFeatures(on))
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app");
            let features = app_features(app.handle().clone()).await.expect("features");
            assert_eq!(features.dev_features, on);
        }
    }
}
