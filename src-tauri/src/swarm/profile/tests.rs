//! Unit tests for the `.md` profile loader. Moved verbatim out of the
//! former monolithic `profile.rs` when it was split into the
//! `types` / `parser` / `registry` package (behaviour unchanged).

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::parser::{parse_allowed_tools, parse_profile};
use super::registry::ProfileRegistry;
use super::types::{PermissionMode, ProfileSource};

/// Acceptance: `load_term` returns the 9 bundled terminal-swarm
/// personas with the exact agent ids the hierarchy graph expects.
/// Confirms the new `src/swarm/agents/term/*.md` files are
/// embedded + parse cleanly + their ids match the AGENT_IDS table
/// in `swarm_term::hierarchy`.
#[test]
fn load_term_returns_nine_personas() {
    let registry = ProfileRegistry::load_term(None).expect("load_term");
    let expected_ids = [
        "orchestrator",
        "coordinator",
        "scout",
        "planner",
        "backend-builder",
        "frontend-builder",
        "backend-reviewer",
        "frontend-reviewer",
        "integration-tester",
    ];
    assert_eq!(registry.profile_count(), expected_ids.len());
    for id in expected_ids {
        let p = registry.get(id).unwrap_or_else(|| {
            panic!("term persona {id} missing from registry")
        });
        assert_eq!(p.id, id);
        assert!(!p.body.is_empty(), "term persona {id} body empty");
    }
}

/// Pins the 2026-05-13 autonomy fix: orchestrator + coordinator
/// personas must include the explicit "no mid-execution
/// approval" rules. If a future edit accidentally reverts these,
/// the swarm regresses to the "should I run P0?" pause loop the
/// user reported.
#[test]
fn term_personas_contain_autonomy_rules() {
    let registry = ProfileRegistry::load_term(None).expect("load_term");

    // Orchestrator: Phase 3 must auto-start (no user approval).
    let orch = registry.get("orchestrator").expect("orchestrator");
    assert!(
        orch.body.contains("OTOMATİK BAŞLAR"),
        "orchestrator persona missing 'Faz 3 — OTOMATİK BAŞLAR' \
         rule — regression of 2026-05-13 autonomy fix. body \
         head: {}",
        &orch.body[..orch.body.len().min(400)]
    );
    assert!(
        orch.body.contains("Onay isteme reflekslerini"),
        "orchestrator persona missing 'suppress approval reflex' \
         clause"
    );

    // Coordinator: must have the "execute without approval" rule
    // and the stand-down reset clause.
    let coord = registry.get("coordinator").expect("coordinator");
    assert!(
        coord.body.contains("ONAYI BEKLEMEDEN EXECUTE ET"),
        "coordinator persona missing 'ONAYI BEKLEMEDEN EXECUTE \
         ET' rule — regression of 2026-05-13 autonomy fix."
    );
    assert!(
        coord.body.contains("stand-down")
            || coord.body.contains("Stand-down"),
        "coordinator persona missing stand-down reset clause"
    );
}

/// Pins the 2026-05-13 example-leak fix, carried through the
/// 2026-05-15 file-IPC cutover: orchestrator + coordinator example
/// dispatches must use `<EXAMPLE/...>` placeholders for file paths
/// AND every fenced example must be labelled "ÖRNEK BODY" so
/// claude recognises it as illustrative rather than dispatchable.
/// If someone reverts these defenses, swarm regresses to
/// copy-pasting persona examples as real dispatches (the original
/// bug: frontend-builder implemented the example session-timer
/// dispatch verbatim instead of the user's actual Workflow+models
/// task).
///
/// The pre-2026-05-15 assertion checked `&gt;&gt;` HTML-escapes on
/// the examples, since literal `>>` at column 0 used to fire a
/// PTY-marker route at persona-injection time. The file-IPC
/// design parses no PTY text, so the chevron escape is no longer
/// load-bearing and was dropped.
#[test]
fn term_personas_examples_use_warning_marker_and_placeholders() {
    let registry = ProfileRegistry::load_term(None).expect("load_term");

    for id in ["orchestrator", "coordinator"] {
        let p = registry.get(id).unwrap_or_else(|| {
            panic!("term persona {id} missing")
        });
        assert!(
            p.body.contains("<EXAMPLE/"),
            "{id} persona missing `<EXAMPLE/...>` path \
             placeholders — regression of 2026-05-13 \
             example-leak fix"
        );
        assert!(
            p.body.contains("ÖRNEK BODY"),
            "{id} persona missing 'ÖRNEK BODY' warning label on \
             example blocks — without it claude treats the body \
             inside the fenced block as a real dispatch \
             (regression of 2026-05-13 example-leak fix)"
        );
    }

    // Orchestrator must also carry the incident-citation
    // anti-pattern so future readers know WHY the defenses exist.
    let orch = registry.get("orchestrator").expect("orchestrator");
    assert!(
        orch.body.contains("2026-05-13"),
        "orchestrator persona missing 2026-05-13 incident \
         citation in anti-patterns — the defenses look arbitrary \
         without it"
    );
}

/// Acceptance: load the bundled `scout.md` via the embedded
/// registry path and assert all nine fields land in `Profile`.
#[test]
fn frontmatter_round_trip() {
    let registry = ProfileRegistry::load_from(None).expect("load");
    let scout = registry.get("scout").expect("scout exists");
    assert_eq!(scout.id, "scout");
    assert_eq!(scout.version, "1.0.0");
    assert_eq!(scout.role, "Scout");
    assert!(scout.description.contains("Read-only"));
    assert_eq!(
        scout.allowed_tools,
        vec!["Read".to_string(), "Grep".to_string(), "Glob".to_string()]
    );
    assert_eq!(scout.permission_mode, PermissionMode::Plan);
    // W3-12h bumped Scout max_turns from 6 to 10 after the
    // first frontend integration smoke (frontend-only doc-edit
    // goal) had Scout exhaust its 6-turn budget on Glob+Read+
    // formatting. The 2026-05-07 smoke pass found 10 still tight
    // for frontend goals (Scout exhausted on a TSX investigation),
    // so it was bumped again to 14. Cost not a concern per the
    // owner's quality-first directive (2026-05-06).
    assert_eq!(scout.max_turns, 14);
    assert!(!scout.body.is_empty());
    // source_path is diagnostics-only — assert it points into the
    // <bundled> virtual prefix, not a host path.
    assert!(scout
        .source_path
        .to_string_lossy()
        .contains("<bundled>"));
    assert_eq!(
        registry.source("scout"),
        Some(ProfileSource::Bundled)
    );
}

/// All nine bundled profiles load — the same gate the registry
/// passes on a clean install. W3-12g renamed `reviewer` to
/// `backend-reviewer` and added `frontend-builder` +
/// `frontend-reviewer` to the bundle so the Coordinator's
/// scope classification has a full backend/frontend roster
/// available (FSM dispatch still uses the backend chain in
/// 12g; 12h activates scope-aware dispatch). W3-12k1 added the
/// 9th and final agent (`orchestrator`) — the user-facing
/// routing brain that sits *above* Coordinator, inserted
/// alphabetically between `integration-tester` and `planner`.
#[test]
fn bundled_nine_profiles_present() {
    let registry = ProfileRegistry::load_from(None).expect("load");
    let mut ids: Vec<&str> = registry
        .list()
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "backend-builder",
            "backend-reviewer",
            "coordinator",
            "frontend-builder",
            "frontend-reviewer",
            "integration-tester",
            "orchestrator",
            "planner",
            "scout",
        ]
    );
}

/// Nine bundled profiles must have distinct ids — duplicates
/// inside the bundled set would surface as a hard load error,
/// but a separate sanity test catches future mistakes earlier
/// (e.g. someone copy-pastes a frontmatter block and forgets to
/// change the `id:` field).
#[test]
fn bundled_nine_profiles_have_distinct_ids() {
    let registry = ProfileRegistry::load_from(None).expect("load");
    let ids: Vec<String> = registry
        .list()
        .iter()
        .map(|p| p.id.clone())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        ids.len(),
        sorted.len(),
        "duplicate ids in bundled profile set: {ids:?}"
    );
    assert_eq!(ids.len(), 9, "expected 9 bundled profiles, got {ids:?}");
}

/// Every bundled `.md` parses cleanly. Implicit in
/// `bundled_nine_profiles_present`, but the dedicated probe
/// surfaces frontmatter bugs in a single profile file faster
/// than the aggregate test (which fails on the first parse
/// error, not the count assertion).
#[test]
fn bundled_nine_profiles_load_without_error() {
    let registry =
        ProfileRegistry::load_from(None).expect("load all bundled");
    for id in [
        "backend-builder",
        "backend-reviewer",
        "coordinator",
        "frontend-builder",
        "frontend-reviewer",
        "integration-tester",
        "orchestrator",
        "planner",
        "scout",
    ] {
        let profile = registry
            .get(id)
            .unwrap_or_else(|| panic!("bundled profile `{id}` missing"));
        assert!(
            !profile.body.is_empty(),
            "bundled profile `{id}` has empty body"
        );
        assert_eq!(profile.id, id);
    }
}

#[test]
fn missing_id_rejected() {
    let raw = "---\nversion: 1.0.0\nrole: r\ndescription: d\n---\nbody";
    let err = parse_profile(raw, PathBuf::from("test.md"))
        .expect_err("missing id rejected");
    assert_eq!(err.kind(), "invalid_input");
    assert!(err.message().contains("missing required frontmatter field `id`"));
}

/// WP §7 — multi-paragraph body comes back verbatim, blank lines
/// included.
#[test]
fn body_preserves_blank_lines() {
    let raw = "---\nid: foo\nversion: 1.0.0\nrole: r\ndescription: d\n---\n# Foo\n\nFirst paragraph.\n\nSecond paragraph.\n";
    let profile = parse_profile(raw, PathBuf::from("foo.md")).unwrap();
    assert!(profile.body.contains("First paragraph."));
    assert!(profile.body.contains("Second paragraph."));
    assert!(profile.body.contains("\n\nFirst"));
    assert!(profile.body.contains("\n\nSecond"));
}

/// WP §7 — id rule: leading lowercase letter, 2..=41 chars,
/// `[a-z0-9-]` after.
#[test]
fn id_validation_rules() {
    let make = |id: &str| {
        format!(
            "---\nid: {id}\nversion: 1.0.0\nrole: r\ndescription: d\n---\n"
        )
    };
    // Rejected:
    for bad in ["Foo", "1foo", "a", &"a".repeat(50)] {
        let raw = make(bad);
        let err = parse_profile(&raw, PathBuf::from("x.md"))
            .expect_err(&format!("`{bad}` should be rejected"));
        assert_eq!(err.kind(), "invalid_input");
    }
    // Accepted:
    for ok in ["scout", "a-b", "a1"] {
        let raw = make(ok);
        parse_profile(&raw, PathBuf::from("x.md"))
            .unwrap_or_else(|e| panic!("`{ok}` rejected: {e:?}"));
    }
}

/// WP §7 — workspace dir overrides bundled when ids collide.
#[test]
fn workspace_overrides_bundled() {
    let dir = TempDir::new().expect("tempdir");
    // Drop a `scout.md` that contradicts the bundled scout: a
    // different role string is enough to prove the override won.
    let body = "---\nid: scout\nversion: 9.9.9\nrole: ReplacedScout\ndescription: workspace override\n---\nWorkspace body.\n";
    std::fs::write(dir.path().join("scout.md"), body).unwrap();

    let registry =
        ProfileRegistry::load_from(Some(dir.path())).expect("load");
    let scout = registry.get("scout").expect("scout exists");
    assert_eq!(scout.role, "ReplacedScout");
    assert_eq!(scout.version, "9.9.9");
    assert_eq!(
        registry.source("scout"),
        Some(ProfileSource::Workspace)
    );
    // Bundled siblings are still present (planner / backend-builder
    // didn't have workspace overrides).
    assert!(registry.get("planner").is_some());
    assert!(registry.get("backend-builder").is_some());
}

/// `allowed_tools` accepts both quoted and unquoted (YAML flow)
/// forms.
#[test]
fn allowed_tools_accepts_both_forms() {
    let quoted = parse_allowed_tools(
        r#"["Read", "Edit", "Bash(cargo *)"]"#,
        Path::new("x.md"),
    )
    .unwrap();
    assert_eq!(quoted, vec!["Read", "Edit", "Bash(cargo *)"]);

    let unquoted =
        parse_allowed_tools("[Read, Edit, Glob]", Path::new("x.md"))
            .unwrap();
    assert_eq!(unquoted, vec!["Read", "Edit", "Glob"]);
}

/// Missing `allowed_tools:` defaults to `["Read"]` per WP §2.
#[test]
fn allowed_tools_defaults_to_read() {
    let raw = "---\nid: foo\nversion: 1.0.0\nrole: r\ndescription: d\n---\n";
    let profile = parse_profile(raw, PathBuf::from("foo.md")).unwrap();
    assert_eq!(profile.allowed_tools, vec!["Read".to_string()]);
}

/// Permission-mode parser tolerates both kebab and camel.
#[test]
fn permission_mode_accepts_camel_and_kebab() {
    for (form, expected) in [
        ("plan", PermissionMode::Plan),
        ("acceptEdits", PermissionMode::AcceptEdits),
        ("accept-edits", PermissionMode::AcceptEdits),
        ("acceptAll", PermissionMode::AcceptAll),
        ("accept-all", PermissionMode::AcceptAll),
    ] {
        let parsed = PermissionMode::parse(form, Path::new("x.md"))
            .unwrap_or_else(|e| {
                panic!("`{form}` should parse: {e:?}")
            });
        assert_eq!(parsed, expected, "form `{form}`");
    }
    let err = PermissionMode::parse("garbage", Path::new("x.md"))
        .expect_err("garbage rejected");
    assert_eq!(err.kind(), "invalid_input");
}
