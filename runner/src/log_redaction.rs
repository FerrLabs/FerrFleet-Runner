use std::io::{self, Write};
use std::sync::{Arc, LazyLock};

use tracing_subscriber::fmt::MakeWriter;

use crate::redactor::{Redactor, SharedRedactor};

static PROCESS: LazyLock<SharedRedactor> = LazyLock::new(SharedRedactor::default);

pub fn shared() -> SharedRedactor {
    PROCESS.clone()
}

pub struct RedactingMakeWriter<M> {
    redactor: SharedRedactor,
    inner: M,
}

impl<M> RedactingMakeWriter<M> {
    pub fn new(redactor: SharedRedactor, inner: M) -> Self {
        Self { redactor, inner }
    }
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for RedactingMakeWriter<M> {
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            redactor: self.redactor.current(),
            inner: self.inner.make_writer(),
        }
    }
}

pub struct RedactingWriter<W> {
    redactor: Arc<Redactor>,
    inner: W,
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        self.inner
            .write_all(self.redactor.log_line(&text).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::prelude::*;

    use super::RedactingMakeWriter;
    use crate::redactor::{Secret, SharedRedactor};
    use crate::test_support::{Rng, fake_value};

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn logged(redactor: &SharedRedactor, emit: impl FnOnce()) -> String {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(RedactingMakeWriter::new(redactor.clone(), captured.clone())),
        );
        tracing::subscriber::with_default(subscriber, emit);
        String::from_utf8(captured.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn a_run_token_in_a_raw_error_log_never_reaches_the_output_and_the_line_stays_json() {
        let token = fake_value(100);
        let quoted = format!("{}\"quoted", fake_value(101));
        let redactor = SharedRedactor::default();
        redactor.reset(vec![
            Secret::new("FERRFLEET_RUN_TOKEN", &token).unwrap(),
            Secret::new("DB_PASSWORD", &quoted).unwrap(),
        ]);
        let err = anyhow::anyhow!("401 with {token}").context(format!("calling with {quoted}"));

        let out = logged(&redactor, || {
            tracing::error!(?err, "run failed in the runner");
        });

        assert!(!out.contains(&token), "{out}");
        assert!(!out.contains(&fake_value(101)), "{out}");
        let line: serde_json::Value = serde_json::from_str(out.trim()).expect("one JSON line");
        assert_eq!(line["fields"]["message"], "run failed in the runner");
        assert!(
            line["fields"]["err"]
                .as_str()
                .unwrap()
                .contains("<redacted>")
        );
    }

    #[test]
    fn a_secret_added_during_the_run_is_redacted_from_then_on() {
        let redactor = SharedRedactor::default();
        let token = fake_value(102);

        let before = logged(&redactor, || tracing::info!(value = %token, "before"));
        redactor.add(vec![Secret::new("X-Api-Key", &token).unwrap()]);
        let after = logged(&redactor, || tracing::info!(value = %token, "after"));

        assert!(before.contains(&token));
        assert!(!after.contains(&token), "{after}");
    }

    #[test]
    fn logs_without_secrets_pass_through_unchanged() {
        let redactor = SharedRedactor::default();
        redactor.reset(vec![Secret::new("T", &fake_value(103)).unwrap()]);
        let out = logged(&redactor, || {
            tracing::info!(path = "/x/y", "Basic plan: Bearer of news");
        });
        assert!(out.contains("Basic plan: Bearer of news"), "{out}");
    }

    const ODD: &[&str] = &[
        "\"", "\\", "\t", "\n", "\r", "\u{1}", "\u{7f}", "'", "é", "日本", "<", "/",
    ];

    fn odd_secret(rng: &mut Rng) -> String {
        let mut value = String::new();
        while value.chars().count() < 8 + rng.below(16) {
            if rng.below(3) == 0 {
                value.push_str(rng.pick(ODD));
            } else {
                value.push(rng.alnum());
            }
        }
        value
    }

    fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
            serde_json::Value::Object(map) => map.iter().for_each(|(k, v)| {
                out.push(k.clone());
                strings(v, out);
            }),
            _ => {}
        }
    }

    #[test]
    fn every_redacted_log_line_stays_json_and_hides_odd_secrets_in_every_field_style() {
        let mut rng = Rng(0x10_6405);
        let (mut leaks, mut invalid, mut lines) = (0, 0, 0);
        for _ in 0..1_000 {
            let secret = odd_secret(&mut rng);
            let Some(registered) = Secret::new("DB_PASSWORD", &secret) else {
                continue;
            };
            let debug = format!("{secret:?}");
            let debug = &debug[1..debug.len() - 1];
            let redactor = SharedRedactor::default();
            redactor.reset(vec![registered]);
            let err = std::io::Error::other(format!("connect with {secret}"));
            let out = logged(&redactor, || {
                tracing::warn!(?err, "failed");
                tracing::warn!(%err, "failed");
                tracing::warn!(value = ?secret, "failed");
                tracing::warn!("failed with {secret}");
            });
            for line in out.lines() {
                lines += 1;
                let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
                    invalid += 1;
                    continue;
                };
                let mut decoded = Vec::new();
                strings(&parsed, &mut decoded);
                if decoded
                    .iter()
                    .any(|s| s.contains(&secret) || s.contains(debug))
                {
                    leaks += 1;
                }
            }
        }
        assert!(lines > 3_000, "{lines}");
        assert_eq!((leaks, invalid), (0, 0), "of {lines} lines");
    }
}
