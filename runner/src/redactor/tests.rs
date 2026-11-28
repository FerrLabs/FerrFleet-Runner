use std::time::{Duration, Instant};

use ferrfleet_shared::{DeltaKind, ExecutorEvent};
use serde_json::json;
use time::OffsetDateTime;

use super::{MIN_SECRET_CHARS, Redactor, Secret, SharedRedactor};
use crate::test_support::{Rng, fake_token, fake_value};

fn redactor(values: &[&str]) -> Redactor {
    Redactor::new(
        values
            .iter()
            .map(|v| Secret::new("TEST_TOKEN", v).expect("a long enough secret"))
            .collect(),
    )
}

fn every_kind(s: &str) -> Vec<ExecutorEvent> {
    let at = OffsetDateTime::now_utc();
    vec![
        ExecutorEvent::SessionStarted {
            session_id: s.to_owned(),
            model: Some(s.to_owned()),
            timestamp: at,
        },
        ExecutorEvent::AssistantMessage {
            content: s.to_owned(),
            timestamp: at,
        },
        ExecutorEvent::Thinking {
            content: s.to_owned(),
            timestamp: at,
        },
        ExecutorEvent::Delta {
            kind: DeltaKind::Text,
            content: s.to_owned(),
            timestamp: at,
        },
        ExecutorEvent::Delta {
            kind: DeltaKind::Thinking,
            content: s.to_owned(),
            timestamp: at,
        },
        ExecutorEvent::ToolUse {
            tool_name: s.to_owned(),
            tool_use_id: s.to_owned(),
            input: json!({"command": s, "nested": [{"deep": {"k": [1, s]}}], s: true}),
            timestamp: at,
        },
        ExecutorEvent::ToolResult {
            tool_use_id: s.to_owned(),
            output: json!([{"type": "text", "text": s}, {"content": [[s]]}, s]),
            is_error: true,
            timestamp: at,
        },
        ExecutorEvent::Usage {
            input_tokens: 1,
            output_tokens: 2,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 4,
            timestamp: at,
        },
        ExecutorEvent::Error {
            message: s.to_owned(),
            provider_signal: false,
            timestamp: at,
        },
        ExecutorEvent::Completed {
            exit_code: 0,
            session_id: Some(s.to_owned()),
            timestamp: at,
        },
    ]
}

const CONTEXT: &[&str] = &[
    "",
    " ",
    "\n",
    "FERRFLEET_RUN_TOKEN=",
    "Bearer ",
    "Authorization: ",
    "'",
    "\"",
    "https://x-access-token:",
    "@github.com/o/r",
    "日本",
    "é",
    "word",
    "ghs_",
    "<redacted>",
    "\t",
    "/",
    "=",
];

#[test]
fn a_run_token_in_any_string_field_of_any_event_kind_never_survives_serialization() {
    let mut rng = Rng(0x5EED_0405);
    for _ in 0..2_000 {
        let token = rng.sample();
        let redactor = redactor(&[&token]);
        let text = format!(
            "{}{}{token}{}{}",
            rng.pick(CONTEXT),
            rng.pick(CONTEXT),
            rng.pick(CONTEXT),
            rng.pick(CONTEXT)
        );
        for event in every_kind(&text) {
            let wire = serde_json::to_string(&redactor.event(event)).unwrap();
            assert!(!wire.contains(&token), "{text:?} -> {wire}");
        }
    }
}

#[test]
fn every_event_kind_keeps_its_type_and_non_string_fields() {
    let redactor = redactor(&[&fake_value(110)]);
    for event in every_kind("plain") {
        let before = serde_json::to_value(&event).unwrap();
        let after = serde_json::to_value(redactor.event(event)).unwrap();
        assert_eq!(before, after);
    }
}

#[test]
fn ordinary_tool_output_prose_json_diffs_and_remotes_pass_through_byte_for_byte() {
    let redactor = redactor(&[&fake_value(111), &fake_value(112)]);
    let samples = [
        "Basic setup is easy. The Bearer of this note explains basic auth; authorization is checked per request.\nThanks!\n",
        "{\n  \"name\": \"ferrfleet\",\n  \"version\": \"1.2.3\",\n  \"deps\": [\"a\", \"b\"],\n  \"url\": \"https://github.com/FerrLabs/FerrFleet-Runner\"\n}\n",
        "diff --git a/src/x.rs b/src/x.rs\n--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1,3 +1,4 @@\n fn main() {\n-    println!(\"hi\");\n+    println!(\"hello\");\n+    let basic = highs_and_lows();\n }\n",
        "origin\thttps://x-access-token@github.com/o/r.git (fetch)\norigin\thttps://x-access-token@github.com/o/r.git (push)\nupstream\tssh://git@github.com/o/r.git (fetch)\nmirror\thttps://github.com/o/r.git (push)\n",
        "HOME=/home/runner\r\nPATH=/usr/local/bin:/usr/bin\r\nFERRFLEET_RUN_ID=01999999-9999-7999-9999-999999999999\r\n",
        "    indented\t\ttabs  and  double  spaces  \n\n\n",
        "",
    ];
    for text in samples {
        assert_eq!(redactor.output(text), text);
        let value = json!({"stdout": text, "lines": text.lines().collect::<Vec<_>>()});
        assert_eq!(redactor.json(value.clone()), value);
    }
}

#[test]
fn an_environment_dump_never_shows_any_registered_secret() {
    let run_token = fake_value(113);
    let claude = fake_value(114);
    let redactor = redactor(&[&run_token, &claude]);
    let dump = format!(
        "FERRFLEET_API_URL=https://api.example\nFERRFLEET_RUN_TOKEN={run_token}\nCLAUDE_CODE_OAUTH_TOKEN={claude}\nPATH=/bin\n"
    );

    assert_eq!(
        redactor.output(&dump),
        "FERRFLEET_API_URL=https://api.example\nFERRFLEET_RUN_TOKEN=<redacted>\nCLAUDE_CODE_OAUTH_TOKEN=<redacted>\nPATH=/bin\n"
    );
}

#[test]
fn a_secret_inside_a_longer_word_is_replaced_too() {
    let token = fake_value(115);
    let redactor = redactor(&[&token]);
    assert_eq!(
        redactor.output(&format!("prefix{token}suffix")),
        "prefix<redacted>suffix"
    );
}

#[test]
fn the_longest_of_two_overlapping_secrets_wins() {
    let short = fake_value(116);
    let long = format!("{short}-extended");
    let redactor = redactor(&[&short, &long]);
    assert_eq!(
        redactor.output(&format!("a {long} b {short} c")),
        "a <redacted> b <redacted> c"
    );
}

#[test]
fn a_secret_with_json_special_characters_is_caught_in_its_escaped_form() {
    let secret = format!("{}\"q\\b", fake_value(117));
    let redactor = redactor(&[&secret]);
    let escaped = serde_json::to_string(&secret).unwrap();
    assert!(!redactor.exact(&escaped).contains(&fake_value(117)));
    assert_eq!(redactor.output(&secret), "<redacted>");
}

#[test]
fn empty_short_and_marker_values_are_refused() {
    assert!(Secret::new("EMPTY", "").is_none());
    assert!(Secret::new("SHORT", &"x".repeat(MIN_SECRET_CHARS - 1)).is_none());
    assert!(Secret::new("MULTIBYTE", &"é".repeat(MIN_SECRET_CHARS - 1)).is_none());
    assert!(Secret::new("MARKER", "redacted").is_none());
    assert!(Secret::new("MARKER", "<redacted>").is_none());
    assert!(Secret::new("EXACT", &"x".repeat(MIN_SECRET_CHARS)).is_some());
}

#[test]
fn a_secret_never_shows_in_debug_output() {
    let value = fake_value(118);
    let secret = Secret::new("TEST", &value).unwrap();
    assert!(!format!("{secret:?}").contains(&value));
}

#[test]
fn error_events_keep_the_diagnostic_rules_and_content_events_do_not_guess() {
    let redactor = Redactor::default();
    let value = fake_value(119);
    let redacted = |event| serde_json::to_value(redactor.event(event)).unwrap();
    let at = OffsetDateTime::now_utc();

    let error = redacted(ExecutorEvent::Error {
        message: format!("sent Bearer {value}"),
        provider_signal: false,
        timestamp: at,
    });
    let message = redacted(ExecutorEvent::AssistantMessage {
        content: "Bearer tokens and Basic auth are both supported".to_owned(),
        timestamp: at,
    });

    assert_eq!(error["message"], "sent Bearer <redacted>");
    assert_eq!(
        message["content"],
        "Bearer tokens and Basic auth are both supported"
    );
}

#[test]
fn known_token_shapes_are_redacted_without_being_registered() {
    let tok = fake_token("ghs", 125);
    let out = Redactor::default().output(&format!("GITHUB_TOKEN={tok}\nnext"));
    assert_eq!(out, "GITHUB_TOKEN=<redacted>\nnext");
}

#[test]
fn redaction_is_idempotent_and_never_panics_on_arbitrary_text() {
    let mut rng = Rng(0xFACE_0405);
    for _ in 0..5_000 {
        let token = rng.sample();
        let redactor = redactor(&[&token]);
        let pieces: Vec<&str> = (0..rng.below(10)).map(|_| rng.pick(CONTEXT)).collect();
        let at = rng.below(pieces.len() + 1);
        let mut text = pieces[..at].concat();
        text.push_str(&token[..rng.below(token.len() + 1)]);
        text.push_str(&pieces[at..].concat());
        text.push_str(&token);
        let output = redactor.output(&text);
        let diagnostic = redactor.diagnostic(&text);
        assert_eq!(redactor.output(&output), output, "{text:?}");
        assert_eq!(redactor.diagnostic(&diagnostic), diagnostic, "{text:?}");
        assert!(!output.contains(&token) && !diagnostic.contains(&token));
    }
}

#[test]
fn arbitrary_unicode_never_panics() {
    let mut rng = Rng(0xDEC0_DE05);
    let redactor = redactor(&["日本語のトークン", "ascii-secret-1"]);
    for _ in 0..5_000 {
        let text: String = (0..rng.below(40))
            .filter_map(|_| char::from_u32(u32::try_from(rng.below(0x11_0000)).unwrap()))
            .collect();
        let _ = redactor.output(&text);
        let _ = redactor.diagnostic(&text);
    }
}

#[test]
fn a_five_megabyte_tool_output_is_redacted_in_linear_time() {
    let token = fake_value(120);
    let redactor = redactor(&[&token, &fake_value(121), &fake_value(122)]);
    let line = format!(
        "src/lib.rs:12: let basic = \"Bearer\"; url=https://github.com/o/r FERRFLEET_RUN_TOKEN={token}\n"
    );
    let text = line.repeat(5 * 1024 * 1024 / line.len());
    let output = json!([{"type": "text", "text": text}]);

    let started = Instant::now();
    let redacted = redactor.json(output);
    let elapsed = started.elapsed();

    let out = redacted[0]["text"].as_str().unwrap();
    assert!(!out.contains(&token));
    assert_eq!(out.lines().count(), text.lines().count());
    assert!(elapsed < Duration::from_secs(20), "{elapsed:?}");
}

#[test]
fn a_secret_added_to_a_shared_redactor_applies_to_the_next_event_and_keeps_the_old_ones() {
    let shared = SharedRedactor::default();
    let first = fake_value(123);
    let second = fake_value(124);
    shared.reset(vec![Secret::new("A_TOKEN", &first).unwrap()]);

    let before = shared.current();
    shared.add(vec![Secret::new("B_TOKEN", &second).unwrap()]);

    assert_eq!(before.output(&second), second);
    assert_eq!(
        shared.current().output(&format!("{first} {second}")),
        "<redacted> <redacted>"
    );

    shared.reset(Vec::new());
    assert_eq!(shared.current().output(&first), first);
}
