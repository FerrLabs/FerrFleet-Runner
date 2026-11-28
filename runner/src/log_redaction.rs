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
            .write_all(self.redactor.exact(&text).as_bytes())?;
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
    use crate::test_support::fake_value;

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
}
