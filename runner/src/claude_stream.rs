use anyhow::{Context, Result};
use chrono::Utc;
use ferrfleet_shared::ExecutorEvent;
use serde_json::Value;

pub fn translate(line: &str) -> Result<Vec<ExecutorEvent>> {
    let value: Value = serde_json::from_str(line).context("parsing claude json line")?;
    let now = Utc::now();
    let mut out = Vec::new();

    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match kind {
        // Only the `init` system event opens a session. Every other system
        // event carries the same `session_id`, and emitting one per event was
        // producing ~36 `SessionStarted` per run — 46% of the transcript, for
        // a run that has exactly one session.
        "system" if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            if let Some(sid) = value
                .get("session_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                out.push(ExecutorEvent::SessionStarted {
                    session_id: sid.to_string(),
                    model: value
                        .get("model")
                        .and_then(Value::as_str)
                        .filter(|m| !m.is_empty())
                        .map(str::to_string),
                    timestamp: now,
                });
            }
        }
        "assistant" | "user" | "message" => {
            if kind == "user" {
                if let Some(content) = value.get("message").and_then(|m| m.get("content")) {
                    push_user_tool_results(content, now, &mut out);
                }
                return Ok(out);
            }
            let usage_value = value
                .get("message")
                .and_then(|m| m.get("usage"))
                .or_else(|| value.get("usage"));
            if let Some(usage) = usage_value
                && let Some(ev) = usage_event(usage, now)
            {
                out.push(ev);
            }
            if let Some(content) = value.get("message").and_then(|m| m.get("content")) {
                push_assistant_content(content, now, &mut out);
            } else if let Some(text) = value.get("delta").and_then(Value::as_str) {
                out.push(ExecutorEvent::AssistantMessage {
                    content: text.to_string(),
                    timestamp: now,
                });
            }
        }
        "result" => {
            if let Some(usage) = value.get("usage")
                && let Some(ev) = usage_event(usage, now)
            {
                out.push(ev);
            }
            // The CLI's own verdict on whether the underlying API call
            // failed — distinct from the process exit code, and from an
            // application-level refusal, which never sets this at all. But
            // `is_error` alone is not enough: the CLI sets it for every
            // error termination, including an agent that simply exhausts its
            // turn budget, which is purely applicational. Only `provider_signal`
            // (see `is_provider_failure`) decides whether to report the
            // narrower, provider-attributable subset.
            if value.get("is_error").and_then(Value::as_bool) == Some(true) {
                let subtype = value
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let detail = value
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("no detail in result line");
                out.push(ExecutorEvent::Error {
                    message: format!("claude result reported an error ({subtype}): {detail}"),
                    provider_signal: is_provider_failure(subtype, detail),
                    timestamp: now,
                });
            }
        }
        _ => {}
    }

    Ok(out)
}

fn push_assistant_content(
    content: &Value,
    now: chrono::DateTime<Utc>,
    out: &mut Vec<ExecutorEvent>,
) {
    if let Some(items) = content.as_array() {
        for item in items {
            let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
            match item_type {
                "text" => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        out.push(ExecutorEvent::AssistantMessage {
                            content: text.to_string(),
                            timestamp: now,
                        });
                    }
                }
                "tool_use" => {
                    let tool_name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    let tool_use_id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let input = item.get("input").cloned().unwrap_or(Value::Null);
                    out.push(ExecutorEvent::ToolUse {
                        tool_name,
                        tool_use_id,
                        input,
                        timestamp: now,
                    });
                }
                _ => {}
            }
        }
    } else if let Some(text) = content.as_str() {
        out.push(ExecutorEvent::AssistantMessage {
            content: text.to_string(),
            timestamp: now,
        });
    }
}

/// Replace base64 image payloads with a marker before the event is persisted.
///
/// A `tool_result` is cloned verbatim into the event stream. A 1440x900 PNG is
/// roughly 700 kB once base64-encoded, and a UI review run takes ten of them —
/// megabytes of Postgres per run holding bytes nobody reads back.
///
/// This costs the agent nothing: the image was already handed to the model
/// inside the `claude` session, which is where it matters. Only what we store
/// is trimmed, and the media type and encoded (base64) length are kept so the
/// transcript still says an image was there and roughly how big it was.
fn elide_image_payloads(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                elide_image_payloads(item);
            }
        }
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("image") {
                let source = map.get("source");
                let base64_len = source
                    .and_then(|s| s.get("data"))
                    .and_then(Value::as_str)
                    .map_or(0, str::len);
                let media_type = source
                    .and_then(|s| s.get("media_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("image")
                    .to_owned();
                map.insert(
                    "source".to_owned(),
                    serde_json::json!({
                        "type": "elided",
                        "media_type": media_type,
                        "base64_len": base64_len,
                    }),
                );
                return;
            }
            for item in map.values_mut() {
                elide_image_payloads(item);
            }
        }
        _ => {}
    }
}

fn push_user_tool_results(
    content: &Value,
    now: chrono::DateTime<Utc>,
    out: &mut Vec<ExecutorEvent>,
) {
    let Some(items) = content.as_array() else {
        return;
    };
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let tool_use_id = item
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let is_error = item
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut output = item.get("content").cloned().unwrap_or(Value::Null);
        elide_image_payloads(&mut output);
        out.push(ExecutorEvent::ToolResult {
            tool_use_id,
            output,
            is_error,
            timestamp: now,
        });
    }
}

/// Whether an errored `result` line is attributable to the provider rather
/// than to the agent or to our own tooling.
///
/// `is_error` alone is too coarse: the CLI sets it for every error
/// termination, including `error_max_turns` — an agent that simply used up
/// its turn budget, which is purely applicational and happens routinely.
/// `subtype` is not documented as a stable enum either, so this does not
/// trust it alone: it only rules out the one subtype known to be
/// non-provider, then additionally requires the result text to carry an
/// actual API-failure signature (insufficient balance, overloaded, or the
/// provider's own 429/529 status codes) before calling it provider-side.
///
/// When in doubt, this returns `false`. A run wrongly billed and marked
/// failed is a local, legible incident; a circuit wrongly armed queues every
/// organization's runs.
fn is_provider_failure(subtype: &str, detail: &str) -> bool {
    // Exhausting the turn budget is never the provider's fault, regardless
    // of what the result text says.
    if subtype == "error_max_turns" {
        return false;
    }

    let lowered = detail.to_lowercase();
    lowered.contains("credit balance is too low")
        || lowered.contains("overloaded_error")
        || lowered.contains("overloaded")
        || lowered.contains("rate_limit_error")
        || lowered.contains(" 429")
        || lowered.contains(" 529")
}

fn usage_event(usage: &Value, now: chrono::DateTime<Utc>) -> Option<ExecutorEvent> {
    let input = u32_field(usage, "input_tokens");
    let output = u32_field(usage, "output_tokens");
    let cache_creation = u32_field(usage, "cache_creation_input_tokens");
    let cache_read = u32_field(usage, "cache_read_input_tokens");
    if input == 0 && output == 0 && cache_creation == 0 && cache_read == 0 {
        return None;
    }
    Some(ExecutorEvent::Usage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: cache_creation,
        cache_read_input_tokens: cache_read,
        timestamp: now,
    })
}

fn u32_field(value: &Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// Whether this event re-announces the session that is already open.
///
/// Restricting `SessionStarted` to `system/init` removed the bulk of the
/// duplication, but not all of it: Claude re-emits `init` mid-session —
/// after a compaction, for instance — carrying the session id it has
/// already reported. Observed live at three inits, minutes apart, for one
/// session. Each repeat says nothing the first one didn't.
///
/// A genuinely new session id still opens a session, so a run that really
/// runs two of them keeps both boundaries.
#[must_use]
pub fn reopens_current_session(event: &ExecutorEvent, current: Option<&str>) -> bool {
    matches!(
        event,
        ExecutorEvent::SessionStarted { session_id, .. } if current == Some(session_id.as_str())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_system_session_id() {
        let line = r#"{"type":"system","session_id":"abc-123","subtype":"init"}"#;
        let events = translate(line).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutorEvent::SessionStarted { session_id, .. } => {
                assert_eq!(session_id, "abc-123");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn a_repeated_init_does_not_reopen_the_session() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc-123"}"#;
        let event = translate(line).unwrap().remove(0);
        assert!(reopens_current_session(&event, Some("abc-123")));
    }

    #[test]
    fn the_first_init_opens_the_session() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc-123"}"#;
        let event = translate(line).unwrap().remove(0);
        assert!(!reopens_current_session(&event, None));
    }

    /// A run that genuinely starts a second session must keep both
    /// boundaries — deduplicating on the id, not on the variant, is what
    /// makes the transcript usable rather than merely shorter.
    #[test]
    fn a_different_session_id_still_opens_a_session() {
        let line = r#"{"type":"system","subtype":"init","session_id":"def-456"}"#;
        let event = translate(line).unwrap().remove(0);
        assert!(!reopens_current_session(&event, Some("abc-123")));
    }

    #[test]
    fn other_events_are_never_treated_as_session_starts() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        let event = translate(line).unwrap().remove(0);
        assert!(!reopens_current_session(&event, Some("abc-123")));
        assert!(!reopens_current_session(&event, None));
    }

    #[test]
    fn translates_assistant_text() {
        let line = r#"{
            "type":"assistant",
            "message":{
                "content":[{"type":"text","text":"Hello"}],
                "usage":{"input_tokens":10,"output_tokens":5}
            }
        }"#;
        let events = translate(line).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            ExecutorEvent::Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..
            }
        ));
        assert!(
            matches!(&events[1], ExecutorEvent::AssistantMessage { content, .. } if content == "Hello")
        );
    }

    #[test]
    fn translates_tool_use() {
        let line = r#"{
            "type":"assistant",
            "message":{"content":[{
                "type":"tool_use",
                "id":"toolu_1",
                "name":"Read",
                "input":{"file_path":"/etc/hosts"}
            }]}
        }"#;
        let events = translate(line).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutorEvent::ToolUse {
                tool_name,
                tool_use_id,
                input,
                ..
            } => {
                assert_eq!(tool_name, "Read");
                assert_eq!(tool_use_id, "toolu_1");
                assert_eq!(input["file_path"], "/etc/hosts");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn translates_tool_result() {
        let line = r#"{
            "type":"user",
            "message":{"content":[{
                "type":"tool_result",
                "tool_use_id":"toolu_1",
                "content":"127.0.0.1 localhost",
                "is_error":false
            }]}
        }"#;
        let events = translate(line).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutorEvent::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => {
                assert_eq!(tool_use_id, "toolu_1");
                assert!(!is_error);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ignores_unknown_kind() {
        let line = r#"{"type":"weird","foo":1}"#;
        let events = translate(line).unwrap();
        assert!(events.is_empty());
    }

    /// A `result` line's own `is_error` is the only source of a
    /// `provider_signal` error — the API bases its provider-outage handling
    /// on this flag rather than matching free-form message text.
    #[test]
    fn a_result_line_with_a_provider_error_signature_emits_a_provider_signal_error() {
        let line = r#"{
            "type":"result",
            "subtype":"error_during_execution",
            "is_error":true,
            "result":"upstream API error: rate_limit_error, status 429",
            "usage":{"input_tokens":10,"output_tokens":0}
        }"#;
        let events = translate(line).unwrap();
        let error = events
            .iter()
            .find(|e| matches!(e, ExecutorEvent::Error { .. }))
            .expect("an Error event");
        match error {
            ExecutorEvent::Error {
                provider_signal,
                message,
                ..
            } => {
                assert!(*provider_signal);
                assert!(message.contains("429"));
            }
            _ => unreachable!(),
        }
    }

    /// `error_max_turns` is the agent running out of turn budget — purely
    /// applicational, and a common one. It must never arm the circuit,
    /// however the result text happens to be worded.
    #[test]
    fn max_turns_exhaustion_never_sets_the_provider_signal() {
        let line = r#"{
            "type":"result",
            "subtype":"error_max_turns",
            "is_error":true,
            "result":"rate_limit_error mentioned incidentally, status 429 too",
            "usage":{"input_tokens":10,"output_tokens":5}
        }"#;
        let events = translate(line).unwrap();
        let error = events
            .iter()
            .find(|e| matches!(e, ExecutorEvent::Error { .. }))
            .expect("an Error event");
        match error {
            ExecutorEvent::Error {
                provider_signal, ..
            } => {
                assert!(
                    !*provider_signal,
                    "error_max_turns must never be reported as provider-side"
                );
            }
            _ => unreachable!(),
        }
    }

    /// A generic execution error with no API-failure signature in its text
    /// must not be treated as provider-side either: in doubt, don't arm.
    #[test]
    fn an_execution_error_without_a_provider_signature_does_not_set_the_signal() {
        let line = r#"{
            "type":"result",
            "subtype":"error_during_execution",
            "is_error":true,
            "result":"the tool call raised an unexpected exception"
        }"#;
        let events = translate(line).unwrap();
        let error = events
            .iter()
            .find(|e| matches!(e, ExecutorEvent::Error { .. }))
            .expect("an Error event");
        match error {
            ExecutorEvent::Error {
                provider_signal, ..
            } => {
                assert!(!*provider_signal);
            }
            _ => unreachable!(),
        }
    }

    /// A successful result line must never fabricate an error event.
    #[test]
    fn a_successful_result_line_emits_no_error() {
        let line = r#"{
            "type":"result",
            "subtype":"success",
            "is_error":false,
            "usage":{"input_tokens":10,"output_tokens":5}
        }"#;
        let events = translate(line).unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ExecutorEvent::Error { .. }))
        );
    }

    #[test]
    fn captures_the_resolved_model_from_init() {
        // An agent configured with the alias `sonnet` never learns which
        // version ran unless this is captured here.
        let line =
            r#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-sonnet-5"}"#;
        let events = translate(line).unwrap();
        match &events[0] {
            ExecutorEvent::SessionStarted { model, .. } => {
                assert_eq!(model.as_deref(), Some("claude-sonnet-5"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn ignores_system_events_that_are_not_init() {
        // Every system event carries the same session_id. Emitting one
        // SessionStarted each produced ~36 per run — 46% of the transcript.
        let line = r#"{"type":"system","subtype":"compact_boundary","session_id":"s1"}"#;
        assert!(translate(line).unwrap().is_empty());
    }

    #[test]
    fn image_payloads_are_elided_from_tool_results() {
        let line = r#"{
            "type":"user",
            "message":{"content":[{
                "type":"tool_result",
                "tool_use_id":"toolu_shot",
                "content":[{"type":"image","source":{
                    "type":"base64","media_type":"image/png","data":"AAAABBBBCCCC"
                }}],
                "is_error":false
            }]}
        }"#;
        let events = translate(line).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutorEvent::ToolResult { output, .. } => {
                let json = output.to_string();
                assert!(
                    !json.contains("AAAABBBBCCCC"),
                    "base64 payload survived into the persisted event: {json}"
                );
                assert!(
                    json.contains("image/png"),
                    "media type should survive so the transcript still says what it was: {json}"
                );
                assert!(
                    json.contains("\"base64_len\":12"),
                    "the elided length should be recorded: {json}"
                );
            }
            _ => panic!("wrong variant"),
        }
    }

    /// The elision must not disturb ordinary text results, which are the
    /// overwhelming majority and the ones people actually read back.
    #[test]
    fn text_tool_results_are_untouched() {
        let line = r#"{
            "type":"user",
            "message":{"content":[{
                "type":"tool_result",
                "tool_use_id":"toolu_txt",
                "content":[{"type":"text","text":"127.0.0.1 localhost"}],
                "is_error":false
            }]}
        }"#;
        let events = translate(line).unwrap();
        match &events[0] {
            ExecutorEvent::ToolResult { output, .. } => {
                assert!(output.to_string().contains("127.0.0.1 localhost"));
            }
            _ => panic!("wrong variant"),
        }
    }
}
