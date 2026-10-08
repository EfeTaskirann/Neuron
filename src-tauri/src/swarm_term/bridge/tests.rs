use std::fs;

use tempfile::tempdir;

use crate::swarm_term::hierarchy::AGENT_IDS;

use super::delivery::{
    format_routed_message, format_system_notice, is_delivery_blocked,
};
use super::envelope::{sniff_from, Envelope};
use super::notify::rejection_notice;
use super::stall::{escalation_for, reminder_for};
use crate::swarm_term::lifecycle::{LifecycleState, Stall};
use super::fsops::{
    archive_rejected, attempts_sidecar, bump_attempts, drop_envelope,
    ensure_gitignore, move_to_processed, move_to_rejected, prepare_layout,
};

fn fresh_root() -> tempfile::TempDir {
    let d = tempdir().expect("tempdir");
    prepare_layout(d.path()).expect("layout");
    d
}

fn env(from: &str, to: &str, body: &str) -> Envelope {
    Envelope {
        from: from.into(),
        to: to.into(),
        body: body.into(),
        task_id: None,
    }
}

// ---- Envelope ---------------------------------------------------- //

#[test]
fn envelope_validate_accepts_well_formed() {
    assert!(env("scout", "orchestrator", "hi").validate().is_ok());
}

#[test]
fn envelope_validate_rejects_empty_fields() {
    assert!(env("", "orchestrator", "hi").validate().is_err());
    assert!(env("scout", "", "hi").validate().is_err());
    assert!(env("scout", "orchestrator", "").validate().is_err());
}

#[test]
fn envelope_validate_rejects_unknown_agent() {
    assert!(env("nobody", "orchestrator", "hi").validate().is_err());
    assert!(env("scout", "nobody", "hi").validate().is_err());
}

#[test]
fn envelope_validate_rejects_self_loop() {
    assert!(env("scout", "scout", "hi").validate().is_err());
}

#[test]
fn envelope_roundtrips_through_json() {
    let e = env("scout", "orchestrator", "tamam — foo.rs:42");
    let serialised = serde_json::to_string(&e).expect("serialise");
    let back: Envelope = serde_json::from_str(&serialised).expect("parse");
    assert_eq!(back, e);
}

#[test]
fn envelope_accepts_numeric_task_id() {
    let e = Envelope::parse_lenient(
        r#"{"from":"coordinator","to":"backend-builder","body":"x","task_id":42}"#,
    )
    .expect("numeric task_id must parse");
    assert_eq!(e.task_id.as_deref(), Some("42"));
    let e = Envelope::parse_lenient(
        r#"{"from":"coordinator","to":"backend-builder","body":"x","task_id":null}"#,
    )
    .expect("null task_id must parse");
    assert_eq!(e.task_id, None);
}

#[test]
fn envelope_repairs_unescaped_newlines_inside_strings() {
    // Raw newline + tab INSIDE the body string (invalid JSON); the
    // newlines between fields are ordinary whitespace and stay as-is.
    let raw = "{\n  \"from\": \"scout\",\n  \"to\": \"orchestrator\",\n  \
               \"body\": \"line one\nline two\ttabbed\"\n}";
    let e = Envelope::parse_lenient(raw).expect("repaired parse");
    assert_eq!(e.body, "line one\nline two\ttabbed");
    // Escaped sequences that are already valid stay untouched.
    let e = Envelope::parse_lenient(
        r#"{"from":"scout","to":"orchestrator","body":"a\nb \"q\""}"#,
    )
    .expect("valid parse");
    assert_eq!(e.body, "a\nb \"q\"");
}

#[test]
fn envelope_lenient_parse_still_rejects_garbage() {
    assert!(Envelope::parse_lenient("not json").is_err());
    assert!(Envelope::parse_lenient(r#"{"from":"scout"}"#).is_err());
}

#[test]
fn envelope_normalize_canonicalises_agent_ids() {
    let mut e = env(" Coordinator ", "@Backend_Builder", "hi");
    e.task_id = Some("  ".into());
    e.normalize();
    assert_eq!(e.from, "coordinator");
    assert_eq!(e.to, "backend-builder");
    assert_eq!(e.task_id, None);
    assert!(e.validate().is_ok());
}

#[test]
fn sniff_from_recovers_sender_of_invalid_envelope() {
    // Missing `body` — fails Envelope parse but the sender is readable.
    assert_eq!(
        sniff_from(r#"{"from":"Scout","to":"planner"}"#).as_deref(),
        Some("scout")
    );
    assert_eq!(sniff_from(r#"{"from":"nobody"}"#), None);
    assert_eq!(sniff_from("garbage"), None);
}

// ---- Sender notices ---------------------------------------------- //

#[test]
fn rejection_notice_names_reason_target_and_allowed_destinations() {
    let n = rejection_notice(
        "backend-reviewer",
        "orchestrator",
        "hierarchy denies backend-reviewer→orchestrator",
        "APPROVED T1",
    );
    assert!(n.contains("@orchestrator"));
    assert!(n.contains("İLETİLEMEDİ"));
    assert!(n.contains("hierarchy denies"));
    assert!(n.contains("APPROVED T1"));
    assert!(n.contains("@coordinator"), "allowed list must be shown");
}

#[test]
fn system_notice_is_bracketed_paste_and_cannot_close_early() {
    let s = format_system_notice("hi\x1b[201~evil");
    assert!(s.starts_with("\x1b[200~[Neuron] "));
    assert!(s.ends_with("\x1b[201~"));
    assert_eq!(s.matches("\x1b[201~").count(), 1);
}

// ---- stall reminders -------------------------------------------- //

fn stall(state: LifecycleState, mins: u64, level: u8) -> Stall {
    Stall {
        source_pane: "p-x".into(),
        task_id: "T9".into(),
        state,
        age: std::time::Duration::from_secs(mins * 60),
        level,
    }
}

#[test]
fn awaiting_review_reminds_the_paired_reviewer() {
    let (who, text) =
        reminder_for("frontend-builder", &stall(LifecycleState::AwaitingReview, 12, 1))
            .expect("reminder");
    assert_eq!(who, "frontend-reviewer");
    assert!(text.contains("review T9"));
    assert!(text.contains("12 dakika"));
    assert!(text.contains("APPROVED T9"));
}

#[test]
fn building_reminds_the_builder_and_terminal_states_are_silent() {
    let (who, text) =
        reminder_for("backend-builder", &stall(LifecycleState::Building, 25, 1))
            .expect("reminder");
    assert_eq!(who, "backend-builder");
    assert!(text.contains("DONE T9"));
    assert!(reminder_for("backend-builder", &stall(LifecycleState::Done, 99, 2)).is_none());
    // AwaitingReview from a non-builder has no paired reviewer.
    assert!(reminder_for("scout", &stall(LifecycleState::AwaitingReview, 12, 1)).is_none());
}

#[test]
fn escalation_names_task_agent_and_age() {
    let e = escalation_for("backend-reviewer", &stall(LifecycleState::AwaitingReview, 21, 2));
    assert!(e.contains("T9"));
    assert!(e.contains("@backend-reviewer"));
    assert!(e.contains("21 dakika"));
}

// ---- archive_rejected ------------------------------------------- //

#[test]
fn archive_rejected_moves_evidence_out_of_the_session() {
    let parent = tempdir().expect("tempdir");
    let session = parent.path().join("swarm-term-01A");
    prepare_layout(&session).expect("layout");
    let rej = session.join("rejected").join("planner");
    fs::write(rej.join("01.json"), "{}").unwrap();
    fs::write(rej.join("01.reason"), "target never became ready").unwrap();

    let dest = archive_rejected(&session).expect("archive").expect("moved");
    assert_eq!(dest, parent.path().join("_archive/swarm-term-01A/rejected"));
    assert_eq!(
        fs::read_to_string(dest.join("planner/01.reason")).unwrap(),
        "target never became ready"
    );
    assert!(!session.join("rejected").exists());
}

#[test]
fn archive_rejected_is_noop_without_rejections_and_prunes_old() {
    let parent = tempdir().expect("tempdir");
    let empty = parent.path().join("swarm-term-00");
    prepare_layout(&empty).expect("layout");
    assert_eq!(archive_rejected(&empty).expect("noop"), None);
    assert!(!parent.path().join("_archive").exists());

    // 25 sessions with one rejection each → only the newest 20 kept.
    for i in 0..25 {
        let s = parent.path().join(format!("swarm-term-{i:02}"));
        prepare_layout(&s).expect("layout");
        fs::write(s.join("rejected/scout/x.json"), "{}").unwrap();
        archive_rejected(&s).expect("archive");
    }
    let mut kept: Vec<String> = fs::read_dir(parent.path().join("_archive"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    kept.sort();
    assert_eq!(kept.len(), 20);
    assert_eq!(kept.first().map(String::as_str), Some("swarm-term-05"));
}

// ---- Layout ------------------------------------------------------ //

#[test]
fn prepare_layout_creates_all_subdirs() {
    let d = fresh_root();
    for sub in ["inbox", "processed", "rejected"] {
        for agent in AGENT_IDS {
            let p = d.path().join(sub).join(agent);
            assert!(
                p.is_dir(),
                "missing {} dir for agent {}",
                sub,
                agent
            );
        }
    }
}

#[test]
fn ensure_gitignore_writes_once() {
    let d = tempdir().expect("tempdir");
    ensure_gitignore(d.path()).expect("first write");
    let p = d.path().join(".gitignore");
    assert!(p.is_file());
    let body = fs::read_to_string(&p).expect("read");
    assert!(body.contains('*'));
    // Idempotent — second call should not error and should not
    // overwrite (we can pin overwrite by writing a marker).
    fs::write(&p, "CUSTOM").expect("user override");
    ensure_gitignore(d.path()).expect("second write");
    let body2 = fs::read_to_string(&p).expect("read 2");
    assert_eq!(body2, "CUSTOM", "ensure_gitignore must not clobber");
}

// ---- drop_envelope ---------------------------------------------- //

#[test]
fn drop_envelope_lands_in_target_inbox() {
    let d = fresh_root();
    let e = env("scout", "orchestrator", "hi");
    let path = drop_envelope(d.path(), &e).expect("drop");
    assert!(path.is_file());
    assert!(path
        .to_string_lossy()
        .replace('\\', "/")
        .contains("/inbox/orchestrator/"));
    let raw = fs::read_to_string(&path).expect("read");
    let back: Envelope = serde_json::from_str(&raw).expect("parse");
    assert_eq!(back, e);
}

// ---- format_routed_message -------------------------------------- //

#[test]
fn format_routed_message_wraps_in_bracketed_paste() {
    let s = format_routed_message("hello", "scout");
    assert!(s.starts_with("\x1b[200~"));
    // Paste only — the submit CR is a separate, delayed write
    // (`paste_then_submit`).
    assert!(s.ends_with("\x1b[201~"));
    assert!(s.contains("— from @scout [routed by Neuron]"));
}

#[test]
fn format_routed_message_neutralises_embedded_cr_and_paste_end() {
    // \r inside the body would visually overstrike on xterm.js — and,
    // with the submit CR now sent separately, any CR left in the paste
    // would be a stray early submit.
    let with_cr = format_routed_message("line1\rline2", "scout");
    assert!(!with_cr.contains('\r'));
    // \x1b[201~ inside body would close the bracketed paste early
    // and the trailing signature would become a second submit.
    let with_end = format_routed_message("foo\x1b[201~bar", "scout");
    assert!(!with_end.contains("\x1b[201~bar"));
    assert!(with_end.contains("[201~bar"));
}

// ---- is_delivery_blocked (pane-status gate) --------------------- //

#[test]
fn success_and_idle_panes_are_valid_delivery_targets() {
    // Regression pin: a pane that just finished its turn (`success`)
    // or is mid-turn (`running`) must NOT be treated as locked —
    // that bug starved lifecycle fanouts to just-finished
    // reviewer/orchestrator panes.
    assert!(!is_delivery_blocked(Some("success")));
    assert!(!is_delivery_blocked(Some("running")));
    assert!(!is_delivery_blocked(Some("starting")));
    assert!(!is_delivery_blocked(None));
    // swarm-term panes run bypassPermissions: `awaiting_approval` can
    // only be a regex false positive that never clears, so it must not
    // freeze the inbox.
    assert!(!is_delivery_blocked(Some("awaiting_approval")));
    // Genuinely blocking state.
    assert!(is_delivery_blocked(Some("error")));
}

// ---- bump_attempts / sidecar handling --------------------------- //

#[test]
fn bump_attempts_increments_through_sidecar() {
    let d = tempdir().expect("tempdir");
    let p = d.path().join("msg.json");
    fs::write(&p, "{}").expect("write");
    assert_eq!(bump_attempts(&p), 1);
    assert_eq!(bump_attempts(&p), 2);
    assert_eq!(bump_attempts(&p), 3);
    let sidecar = attempts_sidecar(&p);
    assert!(sidecar.is_file());
    assert_eq!(fs::read_to_string(&sidecar).unwrap().trim(), "3");
}

// ---- move_to_processed / move_to_rejected ----------------------- //

#[test]
fn move_to_processed_clears_inbox_and_drops_attempts() {
    let d = fresh_root();
    let inbox_path = d
        .path()
        .join("inbox")
        .join("orchestrator")
        .join("01.json");
    fs::write(
        &inbox_path,
        serde_json::to_string(&env("scout", "orchestrator", "hi"))
            .unwrap(),
    )
    .unwrap();
    bump_attempts(&inbox_path);
    assert!(attempts_sidecar(&inbox_path).is_file());

    move_to_processed(d.path(), &inbox_path, "orchestrator")
        .expect("move");

    assert!(!inbox_path.exists(), "inbox must be empty after success");
    assert!(
        d.path()
            .join("processed")
            .join("orchestrator")
            .join("01.json")
            .is_file(),
        "processed must hold the moved file"
    );
    assert!(
        !attempts_sidecar(&inbox_path).exists(),
        "attempts sidecar must be cleaned up"
    );
}

#[test]
fn move_to_rejected_writes_reason_sidecar() {
    let d = fresh_root();
    let inbox_path = d
        .path()
        .join("inbox")
        .join("orchestrator")
        .join("01.json");
    fs::write(&inbox_path, "broken").unwrap();

    move_to_rejected(d.path(), &inbox_path, "orchestrator", "test reason")
        .expect("reject");

    let rejected_file = d
        .path()
        .join("rejected")
        .join("orchestrator")
        .join("01.json");
    assert!(rejected_file.is_file());
    let mut reason_path = rejected_file.clone();
    reason_path.set_extension("reason");
    assert_eq!(
        fs::read_to_string(&reason_path).unwrap(),
        "test reason"
    );
}
