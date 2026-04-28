fn main() {
    // We supply the Common-Controls v6 manifest below via
    // `embed_test_manifest()` so the SAME manifest reaches both the
    // production exe and the unit-test exes. Tell `tauri-build` to
    // skip its own manifest (otherwise the linker gets two MANIFEST
    // resources and CVTRES errors with LNK1123).
    let attrs = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attrs).expect("tauri-build failed");

    // Why this exists
    // ---------------
    // `tauri-runtime-wry` imports `TaskDialogIndirect` from comctl32.
    // That symbol only exists in Common-Controls v6, which Windows
    // selects via an embedded manifest. Without one, the loader binds
    // against the v5 default and trips `STATUS_ENTRYPOINT_NOT_FOUND`
    // (0xC0000139) the moment a binary referencing `tauri-runtime-wry`
    // tries to run — including all `cargo test` exes built from the
    // `[lib]`.
    //
    // `tauri-build` would normally embed the manifest, but its
    // resource compilation only stamps `[[bin]]` outputs; cargo's lib
    // unit-test exes get nothing. We compile the manifest ourselves
    // and emit `cargo:rustc-link-arg=` (unscoped) so the linker
    // attaches the same `.res` to every output in this crate's tree.
    // The production bin opted out of `tauri-build`'s default manifest
    // above so there is exactly one MANIFEST resource per artifact.
    #[cfg(target_os = "windows")]
    embed_test_manifest();
}

#[cfg(target_os = "windows")]
fn embed_test_manifest() {
    use std::env;
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"));
    let rc_path = manifest_dir.join("test-manifest.rc");
    let res_path = out_dir.join("test-manifest.res");

    let rc_exe = find_rc_exe();
    let status = Command::new(&rc_exe)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status();

    match status {
        Ok(s) if s.success() => {
            // Unscoped: applies to the production bin AND the unit-test
            // exes built from the lib. The bin had its own
            // tauri-build-supplied manifest disabled above, so this is
            // the single source of truth.
            println!("cargo:rustc-link-arg={}", res_path.display());
            println!("cargo:rerun-if-changed=test-manifest.rc");
            println!("cargo:rerun-if-changed=test-manifest.xml");
        }
        Ok(s) => {
            eprintln!(
                "cargo:warning=rc.exe exited with status {s} on test-manifest.rc; tests may STATUS_ENTRYPOINT_NOT_FOUND"
            );
        }
        Err(e) => {
            eprintln!(
                "cargo:warning=could not invoke `rc.exe` to compile test-manifest.rc: {e}; tests may STATUS_ENTRYPOINT_NOT_FOUND"
            );
        }
    }
}

#[cfg(target_os = "windows")]
fn find_rc_exe() -> std::path::PathBuf {
    use std::env;
    use std::path::PathBuf;

    // First, search PATH (Visual Studio Developer prompt sets this).
    if let Ok(path) = env::var("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join("rc.exe");
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // Then walk the Windows SDK install dirs. `WindowsSdkDir` is set
    // by VS, but this build.rs commonly runs outside a developer
    // prompt so we hardcode the canonical paths too.
    let sdk_roots: Vec<PathBuf> = [
        env::var("WindowsSdkDir").ok(),
        Some("C:\\Program Files (x86)\\Windows Kits\\10".to_string()),
        Some("C:\\Program Files\\Windows Kits\\10".to_string()),
    ]
    .into_iter()
    .flatten()
    .map(PathBuf::from)
    .collect();

    for sdk in sdk_roots {
        let bin = sdk.join("bin");
        if let Ok(entries) = std::fs::read_dir(&bin) {
            // SDK versions are named e.g. `10.0.26100.0`. Prefer the
            // newest by sort order.
            let mut versioned: Vec<PathBuf> =
                entries.flatten().map(|e| e.path()).collect();
            versioned.sort();
            versioned.reverse();
            for ver in versioned {
                for arch in ["x64", "x86"] {
                    let candidate = ver.join(arch).join("rc.exe");
                    if candidate.exists() {
                        return candidate;
                    }
                }
            }
        }
    }

    PathBuf::from("rc.exe")
}
