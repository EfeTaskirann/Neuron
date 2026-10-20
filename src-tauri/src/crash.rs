//! Crash reporter: when the app panics, write a sanitized report to a
//! fixed folder and (in a release build) show the user a dialog with a
//! short, repeatable code so they can send it on.
//!
//! The panic hook runs even under `panic = "abort"` (Rust runs the hook,
//! then aborts), so this works in the shipped build. The pure pieces —
//! the stable code, the sanitiser, the report text, the folder path —
//! are unit-tested; the hook, the file write, the dialog and the
//! folder-open are the thin I/O shell around them.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

/// The app identifier, matching `tauri.conf.json` — the crashes folder
/// lives under `%LOCALAPPDATA%\<APP_ID>\crashes`.
const APP_ID: &str = "app.neuron.desktop";

/// Everything the report needs, gathered at crash time. Kept separate
/// from the formatting so the formatter is pure and testable.
pub(crate) struct CrashInfo {
    pub code: String,
    pub version: String,
    pub commit: String,
    pub built: String,
    pub os: String,
    pub location: String,
    pub message: String,
    pub when: String,
}

/// A short, repeatable code for a panic: `NRN-XXXX` (uppercase hex). The
/// SAME underlying bug yields the SAME code on every machine and every
/// build, so a user can quote "NRN-3F2A" and it means one thing. Built
/// from the source location plus a normalised message, so incidental
/// differences (a changing id, a path, a number) don't scatter one bug
/// across many codes.
pub(crate) fn crash_code(location: &str, message: &str) -> String {
    let mut h = DefaultHasher::new();
    location.hash(&mut h);
    normalise_message(message).hash(&mut h);
    format!("NRN-{:04X}", (h.finish() & 0xFFFF) as u16)
}

/// Normalise a panic message so two reports of the same bug hash alike:
/// lowercase, drop runs of digits, and drop anything inside quotes,
/// backticks or parentheses (ids, paths, counts).
fn normalise_message(message: &str) -> String {
    static STRIP: OnceLock<Regex> = OnceLock::new();
    // Quoted/backticked/parenthesised spans, then digit runs.
    let strip = STRIP.get_or_init(|| {
        Regex::new(r#"`[^`]*`|"[^"]*"|'[^']*'|\([^)]*\)|\d+"#).expect("valid regex")
    });
    let stripped = strip.replace_all(message, " ");
    stripped.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Replace every trace of the user's identity in `raw`: the home / user
/// profile directory (either slash form, case-insensitively) becomes
/// `~`, and anything shaped like an email becomes `<email>`. So a report
/// a user sends on carries no username or address.
pub(crate) fn sanitize(raw: &str, profile_dir: &str) -> String {
    let trimmed = profile_dir.trim_end_matches(['\\', '/']);
    let mut out = raw.to_string();
    // Both slash forms of the profile dir → `~`, case-insensitively.
    for needle in [trimmed.replace('/', "\\"), trimmed.replace('\\', "/")] {
        if !needle.is_empty() {
            out = replace_ci(&out, &needle, "~");
        }
    }
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    let email = EMAIL.get_or_init(|| {
        Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").expect("valid regex")
    });
    email.replace_all(&out, "<email>").into_owned()
}

/// Case-insensitive, non-overlapping replace of `needle` in `haystack`.
/// ASCII-case folding only, which is all the profile path needs.
fn replace_ci(haystack: &str, needle: &str, repl: &str) -> String {
    let (hl, nl) = (haystack.to_ascii_lowercase(), needle.to_ascii_lowercase());
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if hl[i..].starts_with(&nl) {
            out.push_str(repl);
            i += needle.len();
        } else {
            // Advance one whole char (ASCII folding keeps byte offsets aligned).
            let ch = haystack[i..].chars().next().expect("char at boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// The folder the report is written to, under the OS's local app-data.
pub(crate) fn crashes_dir(local_app_data: &Path) -> PathBuf {
    local_app_data.join(APP_ID).join("crashes")
}

/// The report text, assembled deterministically from `info`. This is
/// what lands in the file and (trimmed) in the dialog.
pub(crate) fn format_report(info: &CrashInfo) -> String {
    format!(
        "Neuron çöktü\n\
         Kod:    {code}\n\
         Sürüm:  {version} (commit {commit}, {built})\n\
         Sistem: {os}\n\
         Zaman:  {when}\n\
         Konum:  {location}\n\
         Hata:   {message}\n",
        code = info.code,
        version = info.version,
        commit = info.commit,
        built = info.built,
        os = info.os,
        when = info.when,
        location = info.location,
        message = info.message,
    )
}

// ── I/O shell ─────────────────────────────────────────────────────── //
// Side-effecting, platform-specific glue around the pure pieces above.
// Not unit-tested; exercised end to end by `neuron.exe --crash-test`.

use std::sync::atomic::{AtomicBool, Ordering};

/// The first panic does the reporting; a re-entrant or second-thread
/// panic falls straight through, so one crash can't stack dialogs.
static HANDLING: AtomicBool = AtomicBool::new(false);

/// Install the panic hook. Runs even under `panic = "abort"` (Rust runs
/// the hook, then aborts), so the shipped build reports too. Call this
/// first thing in `main`, before anything else can panic.
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !HANDLING.swap(true, Ordering::SeqCst) {
            let location = info
                .location()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "<unknown>".into());
            let payload = info.payload();
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "Box<dyn Any>".into());
            report(&location, &message);
        }
        // Keep the normal behaviour: print to stderr, then abort.
        previous(info);
    }));
}

/// Gather the crash fields, write the report, and (release only) show
/// the dialog and open the folder. Best-effort throughout: a failure
/// here must never get in the way of the panic it is reporting.
fn report(location: &str, message: &str) {
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
    // The code is built from SANITISED text so the same bug yields the
    // same code on every machine (a panic in `C:\Users\<name>\.cargo\…`
    // must not hash per-user).
    let location = sanitize(location, &profile);
    let message = sanitize(message, &profile);
    let info = CrashInfo {
        code: crash_code(&location, &message),
        version: env!("CARGO_PKG_VERSION").to_string(),
        commit: option_env!("NEURON_GIT_COMMIT").unwrap_or("unknown").to_string(),
        built: option_env!("NEURON_BUILD_DATE").unwrap_or("unknown").to_string(),
        os: os_line(),
        location,
        message,
        when: utc_now(),
    };
    let text = format_report(&info);
    let saved = write_report(&info.code, &text);
    show_dialog(&info.code, &text, saved.as_deref());
    if let Some(path) = saved.as_deref() {
        open_folder(path);
    }
}

/// `cmd /c ver` → "Microsoft Windows [Version 10.0.26200.xxxx]".
fn os_line() -> String {
    std::process::Command::new("cmd")
        .args(["/c", "ver"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

/// UTC so a report is unambiguous wherever it was captured; no local
/// timezone lookup (unsound in a multi-threaded process mid-panic).
fn utc_now() -> String {
    chrono::DateTime::from_timestamp(crate::time::now_seconds(), 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_default()
}

/// Write the report under `%LOCALAPPDATA%\<APP_ID>\crashes`; returns the
/// path on success. The filename carries the time and the code.
fn write_report(code: &str, text: &str) -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let dir = crashes_dir(&base);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("crash-{}-{code}.txt", crate::time::now_seconds()));
    std::fs::write(&path, text).ok()?;
    Some(path)
}

/// Skip the GUI in a dev build (the panic is already on the console) and
/// when `NEURON_CRASH_SILENT` is set (the `--crash-test` automated run).
fn silent() -> bool {
    cfg!(debug_assertions) || std::env::var_os("NEURON_CRASH_SILENT").is_some()
}

#[cfg(windows)]
fn show_dialog(code: &str, text: &str, saved: Option<&Path>) {
    if silent() {
        return;
    }
    let mut body = text.to_string();
    if let Some(path) = saved {
        body.push_str(&format!("\nRapor kaydedildi:\n{}", path.display()));
    }
    body.push_str("\n\nBu kodu ve raporu geliştiriciye iletebilirsin. Tamam'a basınca rapor klasörü açılır.");
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(format!("Neuron çöktü — {code}"))
        .set_description(body)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(windows)]
fn open_folder(path: &Path) {
    if silent() {
        return;
    }
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}

#[cfg(not(windows))]
fn show_dialog(_code: &str, _text: &str, _saved: Option<&Path>) {}

#[cfg(not(windows))]
fn open_folder(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_code_is_nrn_plus_four_hex_and_repeatable() {
        let a = crash_code("src/setup.rs:12", "boom");
        assert!(
            a.len() == 8 && a.starts_with("NRN-") && a[4..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()),
            "unexpected shape: {a}"
        );
        // Same inputs → same code, every time (cross-run stable hash).
        assert_eq!(a, crash_code("src/setup.rs:12", "boom"));
    }

    #[test]
    fn the_same_bug_with_different_numbers_or_ids_shares_a_code() {
        // Incidental differences must not split one bug into many codes.
        assert_eq!(
            crash_code("watcher.rs:40", "pane 3 never became ready"),
            crash_code("watcher.rs:40", "pane 17 never became ready"),
        );
        assert_eq!(
            crash_code("db.rs:9", "migration `0001_init` failed"),
            crash_code("db.rs:9", "migration `0002_seed` failed"),
        );
    }

    #[test]
    fn different_bugs_get_different_codes() {
        assert_ne!(
            crash_code("setup.rs:12", "database is locked"),
            crash_code("watcher.rs:88", "pane never became ready"),
        );
    }

    #[test]
    fn sanitize_strips_the_user_profile_path_in_either_slash_form() {
        let got = sanitize(
            r"panicked at C:\Users\efeta\.cargo\registry\tauri\app.rs:1299",
            r"C:\Users\efeta",
        );
        assert_eq!(got, r"panicked at ~\.cargo\registry\tauri\app.rs:1299");
        // Forward slashes and a different case still match.
        let got = sanitize("read c:/users/efeta/.claude/x", r"C:\Users\efeta");
        assert_eq!(got, "read ~/.claude/x");
    }

    #[test]
    fn sanitize_redacts_emails() {
        assert_eq!(
            sanitize("logged in as jane.doe+test@example.co.uk now", r"C:\Users\x"),
            "logged in as <email> now",
        );
    }

    #[test]
    fn crashes_dir_is_under_the_app_id() {
        let dir = crashes_dir(Path::new(r"C:\Users\efeta\AppData\Local"));
        assert_eq!(dir, PathBuf::from(r"C:\Users\efeta\AppData\Local\app.neuron.desktop\crashes"));
    }

    #[test]
    fn the_report_carries_the_build_id_and_the_error() {
        // The whole point: a report must show which build crashed (the
        // stale-build bug would have been obvious) and the error itself.
        let info = CrashInfo {
            code: "NRN-3F2A".into(),
            version: "0.1.0".into(),
            commit: "abc1234".into(),
            built: "2026-10-20".into(),
            os: "Windows 11 (build 26200)".into(),
            location: "src/bootstrap/setup.rs:123".into(),
            message: "migration 1 was previously applied but has been modified".into(),
            when: "2026-10-20 15:04:11".into(),
        };
        let r = format_report(&info);
        for needle in [
            "NRN-3F2A",
            "0.1.0",
            "abc1234",
            "2026-10-20",
            "Windows 11 (build 26200)",
            "src/bootstrap/setup.rs:123",
            "migration 1 was previously applied but has been modified",
        ] {
            assert!(r.contains(needle), "report is missing {needle:?}:\n{r}");
        }
    }
}
