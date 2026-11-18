//! Checkout attribution for the existing aggregate git_views index. An older
//! writer may replace that index without updating our derived metadata.

use super::dberr;
use crate::core::{
    sha256_hex, GitObjectKind, GitObservation, GitViewCoverage, Result, Ts, MAX_GIT_VIEW_OWNERS,
};
use crate::ingest::deadline::Deadline;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) const KEY_PREFIX: &str = "git:checkout-views:v1:";
const MAX_METADATA_BYTES: usize = 1024 * 1024;

#[derive(Default, Serialize, Deserialize)]
struct Ownership {
    revision: i64,
    checkouts: BTreeMap<String, (String, Ts)>,
    pinned: BTreeSet<String>,
}

impl Ownership {
    fn aggregate(views: &BTreeSet<String>) -> Self {
        Self {
            pinned: views.clone(),
            ..Self::default()
        }
    }

    fn views(&self) -> BTreeSet<String> {
        self.pinned
            .iter()
            .cloned()
            .chain(self.checkouts.values().map(|(oid, _)| oid.clone()))
            .collect()
    }

    fn bounded(&self) -> bool {
        self.checkouts.len() + self.pinned.len() <= MAX_GIT_VIEW_OWNERS
            && self.checkouts.iter().all(|(key, (oid, _))| {
                key.len() == 64
                    && key.bytes().all(|c| c.is_ascii_hexdigit())
                    && crate::ingest::extract::is_oid(oid)
            })
            && self
                .pinned
                .iter()
                .all(|oid| crate::ingest::extract::is_oid(oid))
    }

    fn previous(
        conn: &Connection,
        obs: &GitObservation,
        before: &BTreeSet<String>,
    ) -> Result<Self> {
        let key = format!("{KEY_PREFIX}{}", obs.repository_id);
        let prior: Option<(String, Option<i64>)> = conn
            .query_row(
                "SELECT value,
                        (SELECT max(revision_id) FROM m6_repository_history WHERE repository_id=?2)
                 FROM mind_meta WHERE key=?1 AND length(CAST(value AS BLOB)) <= ?3",
                params![
                    key,
                    obs.repository_id.to_string(),
                    MAX_METADATA_BYTES as i64
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(dberr)?;
        if let Some((json, Some(revision))) = prior {
            if let Ok(owners) = serde_json::from_str::<Self>(&json) {
                if owners.revision == revision && owners.bounded() && owners.views() == *before {
                    return Ok(owners);
                }
            }
        }
        // Legacy, stale or unrecognized attribution cannot displace any tip.
        Ok(Self::aggregate(before))
    }
}

pub(super) struct ViewPlan {
    pub before: BTreeSet<String>,
    pub after: BTreeSet<String>,
    pub checkout_previous: Option<String>,
    owners: Ownership,
}

impl ViewPlan {
    pub(super) fn prepare(
        conn: &Connection,
        obs: &GitObservation,
        deadline: &Deadline,
    ) -> Result<Self> {
        let mut stmt = conn
            .prepare("SELECT commit_oid FROM git_views WHERE repository_id=?1")
            .map_err(dberr)?;
        let mut before = BTreeSet::new();
        for oid in stmt
            .query_map([obs.repository_id.to_string()], |r| r.get::<_, String>(0))
            .map_err(dberr)?
        {
            deadline.check("comparing Git HEAD views")?;
            before.insert(oid.map_err(dberr)?);
        }
        let mut after: BTreeSet<_> = obs.view_commits.iter().cloned().collect();
        let mut previous = None;
        let at = obs.session.observed_at;
        let owners = match &obs.view_coverage {
            GitViewCoverage::Checkout => {
                let mut owners = Ownership::previous(conn, obs, &before)?;
                // The verified receiver supplies one commit and the client
                // binds the root against the broker's actual authorization.
                let [head] = obs.refs.as_slice() else {
                    return Err(crate::core::Error::Storage(
                        "checkout observation requires one HEAD ref".into(),
                    ));
                };
                if head.name != "HEAD"
                    || head.target_kind != GitObjectKind::Commit
                    || !Path::new(&obs.repository_path).is_absolute()
                    || obs.view_commits.as_slice() != std::slice::from_ref(&head.target_oid)
                    || obs.current_commits.as_deref() != Some(obs.view_commits.as_slice())
                    || !obs.ref_events.is_empty()
                    || obs.current_tags.is_some()
                {
                    return Err(crate::core::Error::Storage(
                        "invalid single-checkout Git inventory".into(),
                    ));
                }
                let checkout = sha256_hex(obs.repository_path.as_bytes());
                let old = owners.checkouts.get(&checkout);
                if old.is_some_and(|(_, time)| *time > at) {
                    return Err(crate::core::Error::Storage(
                        "a newer HEAD observation is recorded for this checkout; retry the ingest"
                            .into(),
                    ));
                }
                previous = old.map(|(oid, _)| oid.clone());
                owners
                    .checkouts
                    .insert(checkout, (head.target_oid.clone(), at));
                after = owners.views();
                owners
            }
            GitViewCoverage::Complete { checkouts, pinned } => {
                let known = Ownership {
                    revision: 0,
                    pinned: pinned.clone(),
                    checkouts: checkouts
                        .iter()
                        .map(|(k, oid)| (k.clone(), (oid.clone(), at)))
                        .collect(),
                };
                if known.bounded() && known.views() == after {
                    known
                } else {
                    Ownership::aggregate(&after)
                }
            }
            GitViewCoverage::Aggregate => Ownership::aggregate(&after),
        };
        Ok(Self {
            before,
            after,
            checkout_previous: previous,
            owners,
        })
    }

    pub(super) fn persist(
        mut self,
        conn: &Connection,
        obs: &GitObservation,
        deadline: &Deadline,
    ) -> Result<()> {
        let repository = obs.repository_id.to_string();
        if obs.view_coverage == GitViewCoverage::Checkout {
            for oid in self.before.difference(&self.after) {
                deadline.check("writing Git observations")?;
                conn.execute(
                    "DELETE FROM git_views WHERE repository_id=?1 AND commit_oid=?2",
                    params![repository, oid],
                )
                .map_err(dberr)?;
            }
        } else {
            conn.execute(
                "DELETE FROM git_views WHERE repository_id=?1",
                [&repository],
            )
            .map_err(dberr)?;
        }
        // Observe only the tips this operation actually saw. Other checkout
        // rows keep their original observation timestamps.
        for oid in &obs.view_commits {
            deadline.check("writing Git observations")?;
            conn.execute(
                "INSERT INTO git_views(repository_id,commit_oid,observed_at) VALUES(?1,?2,?3)
                 ON CONFLICT(repository_id,commit_oid) DO UPDATE
                 SET observed_at=max(git_views.observed_at,excluded.observed_at)",
                params![repository, oid, obs.session.observed_at.0],
            )
            .map_err(dberr)?;
        }
        let key = format!("{KEY_PREFIX}{repository}");
        self.owners.revision = conn
            .query_row("SELECT m6_write_revision()", [], |r| r.get(0))
            .map_err(dberr)?;
        if self.owners.bounded() && !self.owners.checkouts.is_empty() {
            let json = serde_json::to_string(&self.owners)
                .map_err(|e| crate::core::Error::Storage(e.to_string()))?;
            if json.len() <= MAX_METADATA_BYTES {
                conn.execute(
                    "INSERT INTO mind_meta(key,value) VALUES(?1,?2)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![key, json],
                )
                .map_err(dberr)?;
                return Ok(());
            }
        }
        conn.execute("DELETE FROM mind_meta WHERE key=?1", [key])
            .map_err(dberr)?;
        Ok(())
    }
}
