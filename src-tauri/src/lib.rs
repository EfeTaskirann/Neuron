// WP-W2-01 Tauri 2 entry point.
// No commands, no plugins, no DB. WP-W2-03 introduces commands;
// WP-W2-02 introduces sqlx + migrations. Keep this minimal.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running Neuron Tauri application");
}
