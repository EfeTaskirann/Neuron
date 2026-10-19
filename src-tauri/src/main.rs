// Prevent a console window from popping up alongside the GUI on
// Windows release builds. Dev builds keep the console for log output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // First thing, before anything else can panic: route panics to the
    // crash reporter (writes a sanitized report, shows a dialog with a
    // short code in release builds).
    neuron_lib::crash::install();

    // `neuron.exe --crash-test` panics on purpose so the whole crash
    // path can be exercised by hand (set NEURON_CRASH_SILENT=1 to skip
    // the dialog and just write the report).
    if std::env::args().any(|a| a == "--crash-test") {
        panic!("crash-test: deliberate panic to verify the crash reporter");
    }

    neuron_lib::run()
}
