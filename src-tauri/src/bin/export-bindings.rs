//! Standalone binary that re-uses `neuron_lib`'s tauri-specta builder
//! to write `app/src/lib/bindings.ts`. Run with either of:
//!
//! ```text
//! cargo run --manifest-path src-tauri/Cargo.toml --bin export-bindings
//! cd src-tauri && cargo run --bin export-bindings
//! ```
//!
//! Per WP-W2-03 the binding file is checked in but never hand-edited.
//! This binary is the single source of truth for regenerating it
//! after any command-surface or model change. The release `tauri dev`
//! flow does not run this binary; it lives here so CI and developers
//! can refresh the file without launching a desktop window.
//!
//! ## Output
//!
//! The output path is anchored on `CARGO_MANIFEST_DIR` (compile-time
//! absolute path to `src-tauri/`), so the binary writes to the right
//! place regardless of where cargo was invoked from. A bare relative
//! path silently lands outside the workspace when `cargo run
//! --manifest-path` keeps CWD at repo root rather than `src-tauri/`.

fn main() {
    use specta_typescript::Typescript;

    let builder = neuron_lib::specta_builder_for_export();

    // Output path is fixed by AGENTS.md §"Path conventions":
    //
    //   `app/src/lib/bindings.ts` — Auto-generated Tauri command
    //   types (specta) — do not edit by hand
    //
    // Any change to the file location requires updating the WP body
    // and AGENTS.md table together.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let target = format!("{manifest_dir}/../app/src/lib/bindings.ts");
    builder
        .export(Typescript::default(), &target)
        .expect("failed to export tauri-specta TypeScript bindings");

    eprintln!("[export-bindings] wrote {target}");
}
