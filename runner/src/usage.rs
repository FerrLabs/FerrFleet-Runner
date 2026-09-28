use std::collections::HashSet;

use chrono::{DateTime, Utc};
use ferrfleet_shared::ExecutorEvent;
use serde_json::Value;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Tokens {
    input: u64,
    output: u64,
    cache_creation: u64,
    cache_read: u64,
}

impl Tokens {
    fn from_usage(usage: &Value) -> Self {
        Self {
            input: u64_field(usage, "input_tokens"),
            output: u64_field(usage, "output_tokens"),
            cache_creation: u64_field(usage, "cache_creation_input_tokens"),
            cache_read: u64_field(usage, "cache_read_input_tokens"),
        }
    }

    fn from_model_usage(models: &serde_json::Map<String, Value>) -> Self {
        models.values().fold(Self::default(), |sum, model| {
            sum.plus(Self {
                input: u64_field(model, "inputTokens"),
                output: u64_field(model, "outputTokens"),
                cache_creation: u64_field(model, "cacheCreationInputTokens"),
                cache_read: u64_field(model, "cacheReadInputTokens"),
            })
        })
    }

    fn plus(self, other: Self) -> Self {
        Self {
            input: self.input.saturating_add(other.input),
            output: self.output.saturating_add(other.output),
            cache_creation: self.cache_creation.saturating_add(other.cache_creation),
            cache_read: self.cache_read.saturating_add(other.cache_read),
        }
    }

    fn saturating_sub(self, other: Self) -> Self {
        Self {
            input: self.input.saturating_sub(other.input),
            output: self.output.saturating_sub(other.output),
            cache_creation: self.cache_creation.saturating_sub(other.cache_creation),
            cache_read: self.cache_read.saturating_sub(other.cache_read),
        }
    }

    fn max(self, other: Self) -> Self {
        Self {
            input: self.input.max(other.input),
            output: self.output.max(other.output),
            cache_creation: self.cache_creation.max(other.cache_creation),
            cache_read: self.cache_read.max(other.cache_read),
        }
    }

    fn into_event(self, timestamp: DateTime<Utc>) -> Option<ExecutorEvent> {
        if self == Self::default() {
            return None;
        }
        Some(ExecutorEvent::Usage {
            input_tokens: saturating_u32(self.input),
            output_tokens: saturating_u32(self.output),
            cache_creation_input_tokens: saturating_u32(self.cache_creation),
            cache_read_input_tokens: saturating_u32(self.cache_read),
            timestamp,
        })
    }
}

#[derive(Debug, Default)]
pub struct UsageLedger {
    counted_messages: HashSet<String>,
    reported: Tokens,
}

impl UsageLedger {
    pub fn on_assistant(&mut self, line: &Value, now: DateTime<Utc>) -> Option<ExecutorEvent> {
        let message = line.get("message")?;
        let usage = message.get("usage")?;
        let id = message.get("id").and_then(Value::as_str)?;
        if !self.counted_messages.insert(id.to_owned()) {
            return None;
        }
        let step = Tokens {
            output: 0,
            ..Tokens::from_usage(usage)
        };
        self.reported = self.reported.plus(step);
        step.into_event(now)
    }

    pub fn on_result(&mut self, line: &Value, now: DateTime<Utc>) -> Option<ExecutorEvent> {
        let total = match line.get("modelUsage").and_then(Value::as_object) {
            Some(models) if !models.is_empty() => Tokens::from_model_usage(models),
            _ => Tokens::from_usage(line.get("usage")?),
        };
        let missing = total.saturating_sub(self.reported);
        self.reported = self.reported.max(total);
        missing.into_event(now)
    }
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn saturating_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tokens(event: Option<ExecutorEvent>) -> (u32, u32, u32, u32) {
        match event {
            Some(ExecutorEvent::Usage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                ..
            }) => (
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            ),
            None => (0, 0, 0, 0),
            Some(other) => panic!("expected a usage event, got {other:?}"),
        }
    }

    fn assistant(id: &str, input: u64, output: u64, cache_read: u64) -> Value {
        json!({
            "type": "assistant",
            "message": {
                "id": id,
                "content": [],
                "usage": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_read_input_tokens": cache_read
                }
            }
        })
    }

    #[test]
    fn lines_of_one_response_are_counted_once() {
        let mut ledger = UsageLedger::default();
        let now = Utc::now();

        let first = ledger.on_assistant(&assistant("msg_1", 100, 1, 2000), now);
        let second = ledger.on_assistant(&assistant("msg_1", 100, 1, 2000), now);
        let third = ledger.on_assistant(&assistant("msg_1", 100, 1, 2000), now);

        assert_eq!(tokens(first), (100, 0, 0, 2000));
        assert!(
            second.is_none(),
            "a tool_use block repeats the text block's usage"
        );
        assert!(third.is_none());
    }

    #[test]
    fn a_step_without_an_id_is_left_to_the_result() {
        let mut ledger = UsageLedger::default();
        let now = Utc::now();
        let anonymous = json!({"type": "assistant", "message": {"usage": {"input_tokens": 100}}});

        assert!(ledger.on_assistant(&anonymous, now).is_none());
        assert!(ledger.on_assistant(&anonymous, now).is_none());
        assert_eq!(
            tokens(ledger.on_result(&json!({"usage": {"input_tokens": 100}}), now)),
            (100, 0, 0, 0)
        );
    }

    #[test]
    fn the_placeholder_output_count_is_not_reported_while_streaming() {
        let mut ledger = UsageLedger::default();

        let step = ledger.on_assistant(&assistant("msg_1", 10, 8, 0), Utc::now());

        assert_eq!(tokens(step).1, 0);
    }

    #[test]
    fn the_result_only_adds_what_the_steps_did_not_report() {
        let mut ledger = UsageLedger::default();
        let now = Utc::now();
        ledger.on_assistant(&assistant("msg_1", 100, 1, 2000), now);
        ledger.on_assistant(&assistant("msg_2", 50, 1, 2100), now);

        let result = json!({
            "type": "result",
            "usage": {"input_tokens": 150, "output_tokens": 420, "cache_read_input_tokens": 4100}
        });

        assert_eq!(tokens(ledger.on_result(&result, now)), (0, 420, 0, 0));
    }

    #[test]
    fn subagent_spend_comes_from_model_usage() {
        let mut ledger = UsageLedger::default();
        let now = Utc::now();
        ledger.on_assistant(&assistant("msg_1", 100, 1, 0), now);

        let result = json!({
            "type": "result",
            "usage": {"input_tokens": 100, "output_tokens": 40},
            "modelUsage": {
                "claude-sonnet-5": {"inputTokens": 100, "outputTokens": 40, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0},
                "claude-haiku-4-5": {"inputTokens": 300, "outputTokens": 90, "cacheReadInputTokens": 500, "cacheCreationInputTokens": 60}
            }
        });

        assert_eq!(
            tokens(ledger.on_result(&result, now)),
            (300, 130, 60, 500),
            "usage excludes subagents, modelUsage does not"
        );
    }

    #[test]
    fn a_run_is_charged_its_total_exactly_once() {
        let mut ledger = UsageLedger::default();
        let now = Utc::now();
        let mut sum = (0, 0, 0, 0);
        let mut add = |event| {
            let (i, o, c, r) = tokens(event);
            sum = (sum.0 + i, sum.1 + o, sum.2 + c, sum.3 + r);
        };
        for line in [
            assistant("msg_1", 100, 1, 2000),
            assistant("msg_1", 100, 1, 2000),
            assistant("msg_2", 50, 1, 2100),
        ] {
            add(ledger.on_assistant(&line, now));
        }
        add(ledger.on_result(
            &json!({"usage": {"input_tokens": 150, "output_tokens": 420, "cache_read_input_tokens": 4100}}),
            now,
        ));

        assert_eq!(sum, (150, 420, 0, 4100));
    }
}
