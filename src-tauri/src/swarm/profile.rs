//! `.md`-backed agent profile loader (WP-W3-11 §2).
//!
//! Profiles are markdown files with a YAML-ish frontmatter block bound
//! by `^---$` lines. The body (everything after the closing `---`) is
//! the persona prompt fed into `claude --append-system-prompt-file`.
//!
//! Two source roots feed the registry, in order (workspace wins on
//! `id` collision per WP §2):
//!
//! 1. `<app_data_dir>/agents/*.md` — user-edited workspace overrides.
//!    Optional; missing dir is not an error.
//! 2. Bundled defaults embedded via `include_dir!` from
//!    `src-tauri/src/swarm/agents/*.md` — three personas
//!    (`scout`, `planner`, `backend-builder`) ship with the binary.
//!
//! Frontmatter is hand-parsed (no `gray_matter` / `serde_yaml` dep —
//! see WP §"Sub-agent reminders"). The contract is intentionally
//! narrow: only the nine fields listed in `Profile` are read; extras
//! are tolerated but ignored so W3-12 can extend the schema without
//! breaking existing profiles.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use include_dir::{include_dir, Dir};
use regex::Regex;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::error::AppError;

/// Bundled defaults — three persona files embedded at compile time.
/// `$CARGO_MANIFEST_DIR` is the `src-tauri/` dir; the path below
/// resolves to `src-tauri/src/swarm/agents/` containing
/// `scout.md`, `planner.md`, and `backend-builder.md`.
static BUNDLED_AGENTS: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/src/swarm/agents");

/// Compiled regexes used by validation. Built once on first access via
/// `OnceLock` so neither test parallelism nor steady-state command
/// dispatch pay the regex-compile cost twice.
fn id_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // Validity rule per WP §2:
        //   `^[a-z][a-z0-9-]{1,40}$`
        // 2..=41 chars total: leading lowercase letter then 1-40
        // lowercase / digit / dash chars. No consecutive-dash rule;
        // future tightening is W3-12's call.
        Regex::new(r"^[a-z][a-z0-9-]{1,40}$").expect("id regex compiles")
    })
}

fn version_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^\d+\.\d+\.\d+$").expect("version regex compiles")
    })
}

/// Permission posture handed to the spawned `claude` subprocess.
///
/// Phase 1 (this WP) treats the value as a binary gate inside
/// `binding::build_specialist_args`:
///
/// - `Plan` → `--permission-mode plan` (no `--dangerously-skip-permissions`).
/// - everything else → `--dangerously-skip-permissions` (so the
///   smoke command can run without a UI prompt).
///
/// W3-12 introduces a per-tool allow / deny mapping; until then the
/// richer `AcceptEdits` / `AcceptAll` distinction is metadata only.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// Read-only / planning posture — `--permission-mode plan`.
    Plan,
    /// Auto-accept Edit / Write tool calls.
    AcceptEdits,
    /// Auto-accept everything including Bash. Phase 1 gate is the same
    /// as `AcceptEdits`; W3-12 splits the two.
    AcceptAll,
}

impl PermissionMode {
    /// Parse a frontmatter `permission_mode:` value. Accepts the three
    /// canonical kebab / camel forms. Errors as `InvalidInput`.
    fn parse(value: &str, source: &Path) -> Result<Self, AppError> {
        match value.trim() {
            "plan" | "Plan" => Ok(Self::Plan),
            "acceptEdits" | "accept-edits" | "accept_edits" => {
                Ok(Self::AcceptEdits)
            }
            "acceptAll" | "accept-all" | "accept_all" => Ok(Self::AcceptAll),
            other => Err(AppError::InvalidInput(format!(
                "{}: unknown permission_mode `{other}`; \
                 expected `plan` | `acceptEdits` | `acceptAll`",
                source.display()
            ))),
        }
    }
}

/// Parsed agent profile. The `body` is the persona prompt passed via
/// `--append-system-prompt-file`; `source_path` is for diagnostics
/// only and never crosses the IPC boundary.
#[derive(Debug, Clone)]
pub struct Profile {
    pub id: String,
    pub version: String,
    pub role: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub permission_mode: PermissionMode,
    pub max_turns: u32,
    pub body: String,
    pub source_path: PathBuf,
}

/// `"bundled"` for profiles embedded via `include_dir!`,
/// `"workspace"` for files read from `<app_data_dir>/agents/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    Bundled,
    Workspace,
}

impl ProfileSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Workspace => "workspace",
        }
    }
}

/// In-memory directory of all loaded profiles. Workspace overrides
/// shadow bundled defaults sharing the same `id`.
pub struct ProfileRegistry {
    profiles: HashMap<String, Profile>,
    sources: HashMap<String, ProfileSource>,
}

impl ProfileRegistry {
    /// Load all profiles. The bundled set is always read; the
    /// workspace dir is read only when supplied and present
    /// (missing dir is not an error per WP §2).
    ///
    /// Workspace files override bundled ones with the same `id`; the
    /// override is logged at `tracing::debug!` level. Duplicate `id`s
    /// **within the same source** are an `InvalidInput` error.
    pub fn load_from(
        workspace_dir: Option<&Path>,
    ) -> Result<Self, AppError> {
        let mut profiles: HashMap<String, Profile> = HashMap::new();
        let mut sources: HashMap<String, ProfileSource> = HashMap::new();

        // 1. Bundled defaults (always available, embedded in binary).
        for file in BUNDLED_AGENTS.files() {
            // Skip non-`.md` files defensively — `include_dir!` only
            // grabs what's on disk, but a future contributor adding a
            // README or .gitkeep would otherwise blow up parsing.
            if file.path().extension().map(|e| e != "md").unwrap_or(true) {
                continue;
            }
            let raw = std::str::from_utf8(file.contents()).map_err(|e| {
                AppError::InvalidInput(format!(
                    "{}: bundled profile is not utf-8: {e}",
                    file.path().display()
                ))
            })?;
            // Bundled profiles use the relative path from the embed
            // root as `source_path` for diagnostics; the prefix
            // `<bundled>/` makes the `bundled` vs. workspace
            // provenance unmistakable in error messages.
            let display = PathBuf::from("<bundled>")
                .join(file.path());
            let profile = parse_profile(raw, display.clone())?;
            // Duplicates *within* the bundled set are a developer bug
            // — fail loudly on startup so it's caught in CI before
            // shipping.
            if profiles.contains_key(&profile.id) {
                return Err(AppError::InvalidInput(format!(
                    "{}: duplicate bundled profile id `{}`",
                    display.display(),
                    profile.id
                )));
            }
            sources.insert(profile.id.clone(), ProfileSource::Bundled);
            profiles.insert(profile.id.clone(), profile);
        }

        // 2. Workspace overrides (optional, file-based).
        if let Some(dir) = workspace_dir {
            if dir.is_dir() {
                let mut seen_in_workspace: HashMap<String, PathBuf> =
                    HashMap::new();
                for entry in std::fs::read_dir(dir).map_err(|e| {
                    AppError::Internal(format!(
                        "read workspace agents dir {}: {e}",
                        dir.display()
                    ))
                })? {
                    let entry = entry.map_err(|e| {
                        AppError::Internal(format!(
                            "iter workspace agents dir {}: {e}",
                            dir.display()
                        ))
                    })?;
                    let path = entry.path();
                    if path.extension().map(|e| e != "md").unwrap_or(true) {
                        continue;
                    }
                    let raw = std::fs::read_to_string(&path).map_err(|e| {
                        AppError::Internal(format!(
                            "read workspace profile {}: {e}",
                            path.display()
                        ))
                    })?;
                    let profile = parse_profile(&raw, path.clone())?;
                    if let Some(prior) = seen_in_workspace.get(&profile.id) {
                        return Err(AppError::InvalidInput(format!(
                            "duplicate workspace profile id `{}` \
                             (first seen at {}, also at {})",
                            profile.id,
                            prior.display(),
                            path.display()
                        )));
                    }
                    seen_in_workspace
                        .insert(profile.id.clone(), path.clone());
                    if profiles.contains_key(&profile.id) {
                        tracing::debug!(
                            id = %profile.id,
                            path = %path.display(),
                            "workspace profile shadows bundled default"
                        );
                    }
                    sources
                        .insert(profile.id.clone(), ProfileSource::Workspace);
                    profiles.insert(profile.id.clone(), profile);
                }
            }
        }

        Ok(Self { profiles, sources })
    }

    /// Look up a profile by id. Returns `None` if neither source
    /// supplied one with this id.
    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.profiles.get(id)
    }

    /// Source provenance for a given id. `None` mirrors `get`'s miss.
    pub fn source(&self, id: &str) -> Option<ProfileSource> {
        self.sources.get(id).copied()
    }

    /// Every profile in the registry. Iteration order is unspecified;
    /// callers that need a stable order sort by `id` themselves.
    pub fn list(&self) -> Vec<&Profile> {
        self.profiles.values().collect()
    }
}

// --------------------------------------------------------------------- //
// Frontmatter parser                                                     //
// --------------------------------------------------------------------- //

/// Parse a single profile from its raw text. `source` is included in
/// every error message so `InvalidInput` payloads point at the
/// offending file.
fn parse_profile(
    raw: &str,
    source: PathBuf,
) -> Result<Profile, AppError> {
    // 1. Frontmatter delimiter scan. The opening `---` must be the
    //    *first* line (after any leading BOM / whitespace lines we
    //    intentionally do not tolerate — keep the format strict so
    //    drift is caught early).
    let mut lines = raw.split('\n');
    let first = lines.next().ok_or_else(|| {
        AppError::InvalidInput(format!(
            "{}: profile is empty",
            source.display()
        ))
    })?;
    if first.trim_end_matches('\r').trim() != "---" {
        return Err(AppError::InvalidInput(format!(
            "{}: missing opening `---` frontmatter delimiter",
            source.display()
        )));
    }

    let mut frontmatter_lines: Vec<&str> = Vec::new();
    let mut closed = false;
    // Track byte offset to slice the body verbatim — preserving blank
    // lines per WP §7's `body_preserves_blank_lines` test.
    let mut consumed_bytes = first.len() + 1; // +1 for the `\n`
    for line in &mut lines {
        consumed_bytes += line.len() + 1;
        if line.trim_end_matches('\r').trim() == "---" {
            closed = true;
            break;
        }
        frontmatter_lines.push(line);
    }
    if !closed {
        return Err(AppError::InvalidInput(format!(
            "{}: missing closing `---` frontmatter delimiter",
            source.display()
        )));
    }

    // Body is everything after the closing `---` line. We slice from
    // the original `raw` so blank lines and exact whitespace round-trip
    // unmodified. Trim a single leading newline for cosmetic parity
    // with how the persona files are authored, but preserve internal
    // blank lines.
    let body_start = consumed_bytes.min(raw.len());
    let body = raw[body_start..].trim_start_matches('\n').to_string();

    // 2. Walk frontmatter key/value pairs. Format:
    //    `<key>: <value>` — value runs to end of line.
    //    Lines starting with `#` or empty are ignored.
    let mut fields: HashMap<String, String> = HashMap::new();
    for raw_line in frontmatter_lines {
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (k, v) = match trimmed.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => {
                return Err(AppError::InvalidInput(format!(
                    "{}: malformed frontmatter line `{trimmed}`",
                    source.display()
                )));
            }
        };
        if k.is_empty() {
            return Err(AppError::InvalidInput(format!(
                "{}: empty frontmatter key in line `{trimmed}`",
                source.display()
            )));
        }
        // First-write-wins: a duplicate key is suspicious enough to
        // surface as a hard error (frontmatter values do not merge).
        if fields.contains_key(k) {
            return Err(AppError::InvalidInput(format!(
                "{}: duplicate frontmatter key `{k}`",
                source.display()
            )));
        }
        fields.insert(k.to_string(), v.to_string());
    }

    // 3. Required fields.
    let id = required(&fields, "id", &source)?;
    if !id_regex().is_match(&id) {
        return Err(AppError::InvalidInput(format!(
            "{}: invalid id `{id}`; must match ^[a-z][a-z0-9-]{{1,40}}$",
            source.display()
        )));
    }
    let version = required(&fields, "version", &source)?;
    if !version_regex().is_match(&version) {
        return Err(AppError::InvalidInput(format!(
            "{}: invalid version `{version}`; must match \\d+\\.\\d+\\.\\d+",
            source.display()
        )));
    }
    let role = required(&fields, "role", &source)?;
    let description = required(&fields, "description", &source)?;

    // 4. Optional fields with documented defaults.
    let allowed_tools = match fields.get("allowed_tools") {
        Some(raw_value) => parse_allowed_tools(raw_value, &source)?,
        None => vec!["Read".to_string()],
    };
    let permission_mode = match fields.get("permission_mode") {
        Some(v) => PermissionMode::parse(v, &source)?,
        None => PermissionMode::Plan,
    };
    let max_turns = match fields.get("max_turns") {
        Some(v) => v.parse::<u32>().map_err(|_| {
            AppError::InvalidInput(format!(
                "{}: invalid max_turns `{v}`; must be a non-negative integer",
                source.display()
            ))
        })?,
        None => 8,
    };

    Ok(Profile {
        id,
        version,
        role,
        description,
        allowed_tools,
        permission_mode,
        max_turns,
        body,
        source_path: source,
    })
}

fn required(
    fields: &HashMap<String, String>,
    key: &str,
    source: &Path,
) -> Result<String, AppError> {
    let v = fields
        .get(key)
        .ok_or_else(|| {
            AppError::InvalidInput(format!(
                "{}: missing required frontmatter field `{key}`",
                source.display()
            ))
        })?
        .trim()
        .to_string();
    if v.is_empty() {
        return Err(AppError::InvalidInput(format!(
            "{}: required frontmatter field `{key}` is empty",
            source.display()
        )));
    }
    Ok(v)
}

/// Parse an `allowed_tools:` line. Accepts both the strict JSON form
/// (`["Read", "Edit"]`) and the unquoted YAML-flow form
/// (`[Read, Edit]`) — we normalise the latter to the former before
/// handing it to `serde_json`. Round-trip a quoted-with-spaces tool
/// name like `"Bash(cargo *)"` survives because we only quote bare
/// identifiers (no spaces, no special chars) when normalising.
fn parse_allowed_tools(
    raw: &str,
    source: &Path,
) -> Result<Vec<String>, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    // Fast path: already valid JSON.
    if let Ok(v) = serde_json::from_str::<Vec<String>>(trimmed) {
        return Ok(v);
    }

    // Slow path: rewrite `[a, b, c]` → `["a","b","c"]` for tokens that
    // are not already double-quoted. We iterate character-by-character
    // with a tiny state machine so commas inside `"..."` don't split
    // a quoted value (e.g. `["Bash(cargo *, pnpm *)"]`).
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(AppError::InvalidInput(format!(
            "{}: allowed_tools must be a `[...]` array, got `{trimmed}`",
            source.display()
        )));
    }
    let inner = &trimmed[1..trimmed.len() - 1];

    let mut tokens: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut in_quotes = false;
    let mut prev_backslash = false;
    let mut paren_depth: i32 = 0;
    for c in inner.chars() {
        match c {
            '\\' if in_quotes && !prev_backslash => {
                buf.push(c);
                prev_backslash = true;
                continue;
            }
            '"' if !prev_backslash => {
                in_quotes = !in_quotes;
                buf.push(c);
            }
            '(' if !in_quotes => {
                paren_depth += 1;
                buf.push(c);
            }
            ')' if !in_quotes => {
                paren_depth -= 1;
                buf.push(c);
            }
            ',' if !in_quotes && paren_depth == 0 => {
                tokens.push(std::mem::take(&mut buf));
            }
            _ => buf.push(c),
        }
        prev_backslash = false;
    }
    if !buf.trim().is_empty() {
        tokens.push(buf);
    }
    if in_quotes {
        return Err(AppError::InvalidInput(format!(
            "{}: allowed_tools has an unterminated quoted string",
            source.display()
        )));
    }

    // Now each `tokens[i]` is either `"already quoted"` or a bare
    // identifier; quote the bare ones and re-parse via serde for
    // canonical handling of escapes.
    let mut json = String::from("[");
    for (i, tok) in tokens.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with('"') && t.ends_with('"') {
            json.push_str(t);
        } else {
            // Wrap in quotes; also escape any embedded backslash /
            // double-quote inside (rare in tool names but keeps the
            // path correct for adversarial inputs).
            json.push('"');
            for ch in t.chars() {
                if ch == '\\' || ch == '"' {
                    json.push('\\');
                }
                json.push(ch);
            }
            json.push('"');
        }
    }
    json.push(']');

    serde_json::from_str::<Vec<String>>(&json).map_err(|e| {
        AppError::InvalidInput(format!(
            "{}: allowed_tools parse failed: {e} \
             (normalised to `{json}`)",
            source.display()
        ))
    })
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
}
