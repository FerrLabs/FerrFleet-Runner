//! Where a run's runner executes, and who is responsible for starting it.

use std::fmt;

/// Who starts the runner for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerMode {
    /// We do. The API creates a Kubernetes Job, which is itself the guarantee
    /// that the run executes once, on our compute, with our Claude credential.
    #[default]
    Managed,
    /// Someone else's pipeline does. We create the run and hand back a token;
    /// no Job exists, the run waits in `pending` until a runner claims it, and
    /// the Claude credential is the operator's own.
    External,
}

impl RunnerMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::External => "external",
        }
    }

    #[must_use]
    pub const fn is_external(self) -> bool {
        matches!(self, Self::External)
    }

    /// Read a value that has already passed the column's CHECK constraint.
    ///
    /// An unrecognised value falls back to `Managed` rather than failing the
    /// read. The constraint makes that unreachable, and the fallback is chosen
    /// so that if it ever is reached it removes capability instead of granting
    /// it: a corrupted row must not become claimable by anyone holding a run
    /// token.
    #[must_use]
    pub fn from_db(raw: &str) -> Self {
        match raw {
            "external" => Self::External,
            _ => Self::Managed,
        }
    }
}

impl fmt::Display for RunnerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_modes_round_trip_through_the_column() {
        for mode in [RunnerMode::Managed, RunnerMode::External] {
            assert_eq!(RunnerMode::from_db(mode.as_str()), mode);
        }
    }

    /// The fallback removes capability. A row we cannot read must not become a
    /// run that any holder of its token may claim and execute elsewhere.
    #[test]
    fn an_unreadable_mode_is_never_external() {
        assert_eq!(RunnerMode::from_db(""), RunnerMode::Managed);
        assert_eq!(RunnerMode::from_db("EXTERNAL"), RunnerMode::Managed);
        assert_eq!(RunnerMode::from_db("externa"), RunnerMode::Managed);
    }

    #[test]
    fn the_default_is_the_behaviour_that_exists_today() {
        assert_eq!(RunnerMode::default(), RunnerMode::Managed);
        assert!(!RunnerMode::default().is_external());
    }

    /// The wire form is the column's form: an agent read over the API and the
    /// same agent read out of Postgres must not disagree about its own mode.
    #[test]
    fn the_serialised_form_matches_the_stored_form() {
        let json = serde_json::to_string(&RunnerMode::External).unwrap();
        assert_eq!(json, "\"external\"");
        assert_eq!(
            serde_json::from_str::<RunnerMode>(&json).unwrap(),
            RunnerMode::External
        );
    }
}
