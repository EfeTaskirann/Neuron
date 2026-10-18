//! The headless swarm is a developer feature (see `crate::features`):
//! every one of its commands refuses to run unless the feature is on.

use crate::commands::swarm::{swarm_profiles_list, swarm_run_job};
use crate::error::AppError;

/// Every `swarm_*` command's first statement is the gate, so none can
/// slip past it — a new command without one fails here.
#[test]
fn every_headless_swarm_command_starts_with_the_gate() {
    const GATE: &str = "crate::features::require_dev_features(&app, \"Headless swarm\")?;";
    let sources = [
        ("agents.rs", include_str!("../agents.rs")),
        ("dispatch.rs", include_str!("../dispatch.rs")),
        ("jobs.rs", include_str!("../jobs.rs")),
        ("orchestrator.rs", include_str!("../orchestrator.rs")),
        ("profiles.rs", include_str!("../profiles.rs")),
        ("run.rs", include_str!("../run.rs")),
    ];
    let mut commands = 0;
    for (file, src) in sources {
        for decl in src.split("pub async fn swarm_").skip(1) {
            let name = decl.split('<').next().unwrap_or(decl);
            let (_, body) = decl
                .split_once("AppError> {")
                .unwrap_or_else(|| panic!("{file}: swarm_{name} has no body"));
            let first = body.trim_start().lines().next().unwrap_or("").trim();
            assert_eq!(first, GATE, "{file}: swarm_{name} does not start with the gate");
            commands += 1;
        }
    }
    assert_eq!(commands, 13, "the headless swarm exposes 13 commands");
}

/// With developer features off — the default — a command is refused
/// before it does anything.
#[tokio::test]
async fn commands_are_refused_while_developer_features_are_off() {
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app");
    let refused = |r: Result<(), AppError>| match r {
        Err(AppError::FeatureDisabled(m)) => assert!(m.contains("NEURON_DEV_FEATURES=1"), "{m}"),
        other => panic!("not refused: {other:?}"),
    };
    refused(swarm_profiles_list(app.handle().clone()).await.map(|_| ()));
    // Refused before the (here unmanaged) job state is even looked up.
    refused(
        swarm_run_job(app.handle().clone(), "ws".into(), "goal".into())
            .await
            .map(|_| ()),
    );
}
