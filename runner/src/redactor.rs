use std::borrow::Cow;
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use aho_corasick::{AhoCorasick, MatchKind};
use ferrfleet_shared::ExecutorEvent;
use serde_json::Value;
use tracing::warn;

use crate::redact::{REDACTED, Rules, redact_with};

pub const MIN_SECRET_CHARS: usize = 8;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Secret(String);

impl Secret {
    pub fn new(source: &str, value: &str) -> Option<Self> {
        if value.chars().count() < MIN_SECRET_CHARS {
            warn!(
                source,
                min_chars = MIN_SECRET_CHARS,
                "not redacting this secret by value: it is shorter than the minimum"
            );
            return None;
        }
        if REDACTED.contains(value) {
            warn!(
                source,
                "not redacting this secret by value: it is part of the redaction marker"
            );
            return None;
        }
        Some(Self(value.to_owned()))
    }

    fn forms(&self) -> Vec<String> {
        let debug = unquoted(&format!("{:?}", self.0));
        let mut forms = vec![
            json_escaped(&self.0),
            json_escaped(&debug),
            debug,
            self.0.clone(),
        ];
        forms.sort();
        forms.dedup();
        forms
    }
}

fn unquoted(quoted: &str) -> String {
    quoted[1..quoted.len() - 1].to_owned()
}

fn json_escaped(text: &str) -> String {
    serde_json::to_string(text).map_or_else(|_| text.to_owned(), |quoted| unquoted(&quoted))
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[derive(Default)]
pub struct Redactor {
    secrets: Vec<Secret>,
    matcher: Option<AhoCorasick>,
}

impl Redactor {
    pub fn new(mut secrets: Vec<Secret>) -> Self {
        secrets.sort();
        secrets.dedup();
        let patterns: Vec<String> = secrets.iter().flat_map(Secret::forms).collect();
        let matcher = (!patterns.is_empty()).then(|| {
            AhoCorasick::builder()
                .match_kind(MatchKind::LeftmostLongest)
                .build(&patterns)
                .expect("a few short literal secrets always fit in an automaton")
        });
        Self { secrets, matcher }
    }

    pub fn log_line<'a>(&self, line: &'a str) -> Cow<'a, str> {
        if self.matcher.is_none() {
            return Cow::Borrowed(line);
        }
        let body = line.trim_end_matches('\n');
        let Ok(parsed) = serde_json::from_str::<Value>(body) else {
            return self.exact(line);
        };
        let redacted = self.exact_json(parsed.clone());
        if redacted == parsed {
            return Cow::Borrowed(line);
        }
        let mut out = redacted.to_string();
        out.push_str(&line[body.len()..]);
        Cow::Owned(out)
    }

    fn exact_json(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.exact(&text).into_owned()),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|v| self.exact_json(v)).collect())
            }
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, v)| (self.exact(&key).into_owned(), self.exact_json(v)))
                    .collect(),
            ),
            other => other,
        }
    }

    pub fn exact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        match &self.matcher {
            Some(matcher) if matcher.is_match(text) => {
                let mut out = String::with_capacity(text.len());
                matcher.replace_all_with(text, &mut out, |_, _, out| {
                    out.push_str(REDACTED);
                    true
                });
                Cow::Owned(out)
            }
            _ => Cow::Borrowed(text),
        }
    }

    pub fn output(&self, text: &str) -> String {
        redact_with(&self.exact(text), Rules::Output)
    }

    pub fn diagnostic(&self, text: &str) -> String {
        redact_with(&self.exact(text), Rules::Diagnostic)
    }

    pub fn json(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.output(&text)),
            Value::Array(items) => Value::Array(items.into_iter().map(|v| self.json(v)).collect()),
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, v)| (self.output(&key), self.json(v)))
                    .collect(),
            ),
            other => other,
        }
    }

    pub fn event(&self, event: ExecutorEvent) -> ExecutorEvent {
        use ExecutorEvent as E;
        match event {
            E::SessionStarted {
                session_id,
                model,
                timestamp,
            } => E::SessionStarted {
                session_id: self.output(&session_id),
                model: model.map(|m| self.output(&m)),
                timestamp,
            },
            E::AssistantMessage { content, timestamp } => E::AssistantMessage {
                content: self.output(&content),
                timestamp,
            },
            E::Thinking { content, timestamp } => E::Thinking {
                content: self.output(&content),
                timestamp,
            },
            E::Delta {
                kind,
                content,
                timestamp,
            } => E::Delta {
                kind,
                content: self.output(&content),
                timestamp,
            },
            E::ToolUse {
                tool_name,
                tool_use_id,
                input,
                timestamp,
            } => E::ToolUse {
                tool_name: self.output(&tool_name),
                tool_use_id: self.output(&tool_use_id),
                input: self.json(input),
                timestamp,
            },
            E::ToolResult {
                tool_use_id,
                output,
                is_error,
                timestamp,
            } => E::ToolResult {
                tool_use_id: self.output(&tool_use_id),
                output: self.json(output),
                is_error,
                timestamp,
            },
            usage @ E::Usage { .. } => usage,
            E::Error {
                message,
                provider_signal,
                timestamp,
            } => E::Error {
                message: self.diagnostic(&message),
                provider_signal,
                timestamp,
            },
            E::Completed {
                exit_code,
                session_id,
                timestamp,
            } => E::Completed {
                exit_code,
                session_id: session_id.map(|s| self.output(&s)),
                timestamp,
            },
        }
    }
}

#[derive(Clone, Default)]
pub struct SharedRedactor(Arc<RwLock<Arc<Redactor>>>);

impl SharedRedactor {
    pub fn current(&self) -> Arc<Redactor> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn reset(&self, secrets: Vec<Secret>) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(Redactor::new(secrets));
    }

    pub fn add(&self, secrets: Vec<Secret>) {
        if secrets.is_empty() {
            return;
        }
        let mut current = self.0.write().unwrap_or_else(PoisonError::into_inner);
        let all = current.secrets.iter().cloned().chain(secrets).collect();
        *current = Arc::new(Redactor::new(all));
    }
}

#[cfg(test)]
mod tests;
