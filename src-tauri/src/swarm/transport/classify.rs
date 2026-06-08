//! Stream-json line classifier — pure, synchronous, subprocess-free so
//! unit tests can drive the parser without spawning a real `claude`.
//!
//! Split out of the former single-file `transport.rs` (DEEP refactor).
//! The spawn/drive side ([`super::subprocess`]) feeds each parsed JSON
//! line through [`classify_event`] and acts on the resulting events.

use serde_json::Value;

use super::event::StreamEvent;

/// Cap on the per-tool-use `input_summary` string fed into the
/// `StreamEvent::ToolUse` event. Long tool inputs (e.g. a giant
/// `Read` of a multi-MiB file path string) would otherwise spam the
/// live-feed UI; ~120 chars is enough for "path: ..." or
/// "pattern: ..., glob: ..." style summaries.
pub(crate) const TOOL_USE_INPUT_SUMMARY_CAP: usize = 120;

/// Classify one parsed JSON event from the `claude` stream-json
/// output into 0 or more `StreamEvent`s. Pulled out as a synchronous
/// helper so unit tests can drive the line parser without spawning a
/// real subprocess.
///
/// Returns:
/// - `vec![]` for ignored / forward-compat events (returned as
///   `Other` semantics — empty vec is the new "skip this line").
/// - 1 event for `system.init` / `result.*`.
/// - N events for `assistant` lines: one `AssistantDelta` if the
///   content has any text blocks (concatenated) PLUS one `ToolUse`
///   per tool_use block. Order matches the order they appear in the
///   `content` array, with text concatenated up-front, so the
///   caller can apply them in stream order.
pub(crate) fn classify_event(value: &Value) -> Vec<StreamEvent> {
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "system" => {
            let subtype = value
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("");
            if subtype == "init" {
                if let Some(id) =
                    value.get("session_id").and_then(Value::as_str)
                {
                    return vec![StreamEvent::SystemInit {
                        session_id: id.to_string(),
                    }];
                }
            }
            vec![]
        }
        "assistant" => classify_assistant_blocks(value),
        "result" => classify_result_event(value),
        _ => vec![],
    }
}

/// `assistant` line classifier. Walks `message.content[]`, concatenates
/// `text` blocks into one `AssistantDelta`, emits one `ToolUse` per
/// `tool_use` block.
fn classify_assistant_blocks(value: &Value) -> Vec<StreamEvent> {
    let blocks = match value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    {
        Some(arr) => arr,
        None => return vec![],
    };
    let mut events: Vec<StreamEvent> = Vec::new();
    let mut text_buf = String::new();
    for block in blocks {
        let block_type =
            block.get("type").and_then(Value::as_str).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(t) = block.get("text").and_then(Value::as_str)
                {
                    text_buf.push_str(t);
                }
            }
            "tool_use" => {
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let input_summary = summarize_tool_input(block.get("input"));
                events.push(StreamEvent::ToolUse {
                    name,
                    input_summary,
                });
            }
            _ => {}
        }
    }
    if !text_buf.is_empty() {
        events.insert(
            0,
            StreamEvent::AssistantDelta { text: text_buf },
        );
    }
    events
}

/// `result` line classifier — unchanged shape vs pre-W4-03; just
/// pulled out for symmetry with `classify_assistant_blocks`.
fn classify_result_event(value: &Value) -> Vec<StreamEvent> {
    let subtype = value
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or("");
    match subtype {
        "success" => {
            let assistant_text = value
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let total_cost_usd = value
                .get("total_cost_usd")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            // The CLI is inconsistent here across versions; we
            // accept either spelling so the transport survives a
            // minor bump.
            let turn_count = value
                .get("num_turns")
                .or_else(|| value.get("turn_count"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                as u32;
            vec![StreamEvent::ResultSuccess {
                assistant_text,
                total_cost_usd,
                turn_count,
            }]
        }
        "error" | "error_max_turns" | "error_during_execution" => {
            let reason = value
                .get("error")
                .and_then(Value::as_str)
                .or_else(|| value.get("message").and_then(Value::as_str))
                .unwrap_or("claude reported a result.error event")
                .to_string();
            vec![StreamEvent::ResultError {
                reason: format!("{subtype}: {reason}"),
            }]
        }
        _ => vec![],
    }
}

/// Build a short "k1: v1, k2: v2" summary of a tool-input JSON
/// object, capped at `TOOL_USE_INPUT_SUMMARY_CAP` chars (with a
/// trailing "…" when truncated).
fn summarize_tool_input(input: Option<&Value>) -> String {
    let input = match input {
        Some(v) => v,
        None => return String::new(),
    };
    let summary = match input {
        Value::Object(map) => {
            let mut parts: Vec<String> = Vec::with_capacity(map.len());
            for (k, v) in map {
                let v_str = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                parts.push(format!("{k}: {v_str}"));
            }
            parts.join(", ")
        }
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    truncate_with_ellipsis(&summary, TOOL_USE_INPUT_SUMMARY_CAP)
}

/// Truncate a string to at most `cap` chars (counted by `chars()`
/// so we don't slice mid-codepoint), append "…" when truncated.
fn truncate_with_ellipsis(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let mut out: String =
        s.chars().take(cap.saturating_sub(1)).collect();
    out.push('…');
    out
}
