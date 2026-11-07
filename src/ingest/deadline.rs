//! A cooperative time limit for one ingest.
//!
//! The limit is checked between the steps of the preparation phase and while
//! waiting for git, which is killed when the limit expires. It is deliberately
//! NOT checked once the database write has started: an interrupted write
//! transaction poisons the store's write fence (an uncertain outcome), so the
//! write phase always runs to its end. See ADR-026, addendum on the ingest
//! time limit.

use std::time::{Duration, Instant};

use crate::core::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    started: Instant,
    limit: Option<Duration>,
    /// Test seam: expire exactly when this phase is checked, so every
    /// checkpoint can be exercised deterministically instead of by timing.
    #[cfg(test)]
    expire_at: Option<&'static str>,
}

impl Deadline {
    /// No limit: the behavior of the CLI.
    pub fn none() -> Self {
        Self {
            started: Instant::now(),
            limit: None,
            #[cfg(test)]
            expire_at: None,
        }
    }

    pub fn after(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            limit: Some(limit),
            #[cfg(test)]
            expire_at: None,
        }
    }

    /// A deadline with a generous limit that expires only when `phase` is
    /// checked (tests only).
    #[cfg(test)]
    pub(crate) fn expiring_at(phase: &'static str) -> Self {
        Self {
            expire_at: Some(phase),
            ..Self::after(Duration::from_secs(3600))
        }
    }

    pub fn limit(&self) -> Option<Duration> {
        self.limit
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn expired(&self) -> bool {
        self.limit.is_some_and(|l| self.started.elapsed() >= l)
    }

    /// Time left, `Duration::ZERO` once expired, `None` without a limit.
    pub fn remaining(&self) -> Option<Duration> {
        self.limit.map(|l| l.saturating_sub(self.started.elapsed()))
    }

    /// Fails when the limit has expired. Call only where nothing has been
    /// written yet: the message states that the database was not changed.
    pub fn check(&self, phase: &str) -> Result<()> {
        self.check_with(phase, "the database was not changed")
    }

    /// Like [`check`](Self::check), with an explicit statement of what was
    /// kept (used between independent ingest roots).
    pub fn check_with(&self, phase: &str, effect: &str) -> Result<()> {
        #[cfg(test)]
        let forced = self.expire_at == Some(phase);
        #[cfg(not(test))]
        let forced = false;
        if forced || self.expired() {
            Err(self.error(phase, effect))
        } else {
            Ok(())
        }
    }

    pub fn error(&self, phase: &str, effect: &str) -> Error {
        let limit = self.limit.map(|l| l.as_secs_f64()).unwrap_or_default();
        Error::IngestTimeout {
            message: format!(
                "ingest stopped after {:.1} s (limit {} s) while {phase}; {effect}. \
                 Select directories with repo_outline and pass them as `paths`, or start the \
                 server with a larger --ingest-timeout.",
                self.elapsed().as_secs_f64(),
                limit
            ),
        }
    }
}
