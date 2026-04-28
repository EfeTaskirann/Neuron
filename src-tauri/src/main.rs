// Prevent a console window from popping up alongside the GUI on
// Windows release builds. Dev builds keep the console for log output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    neuron_lib::run()
}
