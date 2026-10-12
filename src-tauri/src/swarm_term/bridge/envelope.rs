//! The [`Envelope`] inter-agent message wire type and its structural
//! validation. Hierarchy enforcement lives in [`super::watcher`].

use serde::{Deserialize, Deserializer, Serialize};

use crate::swarm_term::hierarchy::AGENT_IDS;

/// One inter-agent message. Agents create one of these via the `Write`
/// tool; the watcher reads it.
///
/// `from` is trust-based: a misbehaving persona could spoof it, but all
/// 9 personas are our own and the cost of path-derived sender attribution
/// (an outbox staging step) is not worth the added complexity in v1.
/// The watcher does cross-check `envelope.to` against the path's target
/// directory and uses the path when they disagree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    pub from: String,
    pub to: String,
    pub body: String,
    /// Accepts a JSON string OR number (`"task_id": 42`) — LLM senders
    /// emit both, and rejecting the number form dropped whole messages.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_task_id"
    )]
    pub task_id: Option<String>,
}

fn de_task_id<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Str(String),
        Num(serde_json::Number),
    }
    Ok(match Option::<Raw>::deserialize(d)? {
        Some(Raw::Str(s)) => Some(s),
        Some(Raw::Num(n)) => Some(n.to_string()),
        None => None,
    })
}

impl Envelope {
    /// Parse an inbox file, tolerating the two shapes LLM senders get
    /// wrong most often: a leading BOM, and raw (unescaped) newlines /
    /// tabs inside string values. The strict parse runs first; the
    /// repaired text is only tried when it fails, and the ORIGINAL
    /// error is returned if both fail (it points at the real problem).
    pub fn parse_lenient(raw: &str) -> Result<Envelope, serde_json::Error> {
        let raw = raw.trim_start_matches('\u{feff}');
        match serde_json::from_str::<Envelope>(raw) {
            Ok(e) => Ok(e),
            Err(strict_err) => match escape_control_chars_in_strings(raw) {
                Some(repaired) => {
                    serde_json::from_str(&repaired).map_err(|_| strict_err)
                }
                None => Err(strict_err),
            },
        }
    }

    /// Canonicalise the agent-id fields: trim, drop a leading `@`,
    /// lowercase, `_`/space → `-`. `"Coordinator"`, `"@coordinator"` and
    /// `"backend_builder"` all name a real agent; rejecting them for
    /// spelling stalled the sender forever.
    pub fn normalize(&mut self) {
        self.from = normalize_agent_id(&self.from);
        self.to = normalize_agent_id(&self.to);
        if let Some(id) = self.task_id.as_mut() {
            *id = id.trim().to_string();
        }
        if self.task_id.as_deref() == Some("") {
            self.task_id = None;
        }
    }

    /// Structural validation. Hierarchy enforcement happens separately.
    pub fn validate(&self) -> Result<(), String> {
        if self.from.trim().is_empty() {
            return Err("from is empty".into());
        }
        if self.to.trim().is_empty() {
            return Err("to is empty".into());
        }
        if !AGENT_IDS.iter().any(|a| *a == self.from) {
            return Err(format!("unknown from agent: {}", self.from));
        }
        if !AGENT_IDS.iter().any(|a| *a == self.to) {
            return Err(format!("unknown to agent: {}", self.to));
        }
        if self.from == self.to {
            return Err("self-loop forbidden".into());
        }
        if self.body.is_empty() {
            return Err("body is empty".into());
        }
        Ok(())
    }
}

pub(super) fn normalize_agent_id(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('@')
        .to_ascii_lowercase()
        .replace(['_', ' '], "-")
}

/// Best-effort sender recovery from a file that does not parse at all,
/// so the rejection notice can still reach the agent that wrote it.
/// Returns the canonicalised id only when it names a real agent.
pub(super) fn sniff_from(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw)
        .ok()
        .or_else(|| {
            escape_control_chars_in_strings(raw)
                .and_then(|r| serde_json::from_str(&r).ok())
        })?;
    let id = normalize_agent_id(v.get("from")?.as_str()?);
    AGENT_IDS.contains(&id.as_str()).then_some(id)
}

/// Escape raw control characters (`\n`, `\r`, `\t`, …) that appear
/// INSIDE JSON string literals — invalid JSON that LLM `Write` calls
/// produce when a multi-line body is not escaped. Whitespace between
/// tokens is left alone. Returns `None` when nothing needed escaping.
fn escape_control_chars_in_strings(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len() + 16);
    let mut in_string = false;
    let mut escaped = false;
    let mut changed = false;
    for ch in raw.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            } else if (ch as u32) < 0x20 {
                changed = true;
                match ch {
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c => out.push_str(&format!("\\u{:04x}", c as u32)),
                }
                continue;
            }
        } else if ch == '"' {
            in_string = true;
        }
        out.push(ch);
    }
    changed.then_some(out)
}
