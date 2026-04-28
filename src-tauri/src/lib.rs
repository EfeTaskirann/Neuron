// WP-W2-01 Tauri 2 entry point.
// WP-W2-02 layered in sqlx, the migrator, and a single `health_db`
// smoke command. Real domain commands (`agents:list`, `runs:list`, …)
// arrive in WP-W2-03; until then this surface is intentionally tiny.

use tauri::Manager;

pub mod commands;
pub mod db;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // `db::init` is async because sqlx is. The Tauri setup
            // hook is sync, so we drive the future via Tauri's own
            // tokio runtime — the same one that hosts every command.
            let handle = app.handle().clone();
            let pool = tauri::async_runtime::block_on(async move {
                db::init(&handle).await
            })?;
            app.manage(pool);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![commands::health::health_db])
        .run(tauri::generate_context!())
        .expect("error while running Neuron Tauri application");
}
