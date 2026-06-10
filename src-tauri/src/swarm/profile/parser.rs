//! Hand-rolled frontmatter parser for agent `.md` profiles.
//!
//! Split out of the former monolithic `profile.rs` (WP-W3-11 §2). No
//! `gray_matter` / `serde_yaml` dependency — the contract is
//! intentionally narrow (only the nine [`Profile`] fields are read;
//! extras are tolerated but ignored so W3-12 can extend the schema
//! without breaking existing profiles).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::error::AppError;

use super::types::{PermissionMode, Profile};

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

/// Parse a single profile from its raw text. `source` is included in
/// every error message so `InvalidInput` payloads point at the
/// offending file.
pub(super) fn parse_profile(
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
pub(super) fn parse_allowed_tools(
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
