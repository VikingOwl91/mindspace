//! A cooperative time limit for one ingest.
//!
//! Checkpoints run during preparation and between persistence work items in the open
//! transaction. A timeout before COMMIT unwinds through the normal rollback path.

use std::time::{Duration, Instant};

use crate::core::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    started: Instant,
    prior_roots: usize,
    writing: bool,
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
            prior_roots: 0,
            writing: false,
            limit: None,
            #[cfg(test)]
            expire_at: None,
        }
    }

    pub fn after(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            prior_roots: 0,
            writing: false,
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

    pub(crate) fn prior_roots(&self) -> usize {
        self.prior_roots
    }

    pub(crate) fn for_root(self, prior_roots: usize) -> Self {
        Self {
            prior_roots,
            ..self
        }
    }

    pub(crate) fn writing(self) -> Self {
        Self {
            writing: true,
            ..self
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

    /// Fails before COMMIT, while the current root can still be discarded.
    pub fn check(&self, phase: &str) -> Result<()> {
        self.check_with(phase, self.effect())
    }

    pub(super) fn effect(&self) -> &'static str {
        if self.writing {
            "the current root was rolled back"
        } else if self.prior_roots > 0 {
            "the current root was not committed"
        } else {
            "the database was not changed"
        }
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
        let kept = if self.prior_roots == 0 {
            String::new()
        } else {
            let (noun, verb) = if self.prior_roots == 1 {
                ("path", "was")
            } else {
                ("paths", "were")
            };
            format!(
                "; {} earlier selected {noun} {verb} committed and kept",
                self.prior_roots
            )
        };
        Error::IngestTimeout {
            writing: self.writing,
            message: format!(
                "ingest stopped after {:.1} s (limit {} s) while {phase}; {effect}{kept}. \
                 Select fewer directories with --path (MCP: `paths` from repo_outline), or increase \
                 --ingest-timeout.",
                self.elapsed().as_secs_f64(),
                limit
            ),
        }
    }
}
