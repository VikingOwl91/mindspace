//! Git source versions stay commit-bound. These links describe the change in
//! the observed HEAD view between two consecutive recordings.

use super::{dberr, supersede_existing_memory, uuid_col, GIT_PRODUCER_FAMILY};
use crate::core::{GitFileReport, GitObjectKind, GitObservation, MemoryActionKind, Result};
use crate::ingest::deadline::Deadline;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;

pub(super) struct HeadMove<'a> {
    previous: String,
    current: &'a str,
}

impl<'a> HeadMove<'a> {
    /// Read the previous HEAD, ingest root and views *before* replacing any
    /// of them. HEAD is repository-global in storage and worktree-local in Git.
    pub(super) fn observed(
        conn: &Connection,
        observation: &'a GitObservation,
        views: &super::git_views::ViewPlan,
    ) -> Result<Option<Self>> {
        let Some(memory_views) = &observation.current_commits else {
            return Ok(None);
        };
        let mut heads = observation.refs.iter().filter(|r| r.name == "HEAD");
        let Some(head) = heads.next() else {
            return Ok(None);
        };
        if heads.next().is_some()
            || head.target_kind != GitObjectKind::Commit
            || !Path::new(&observation.repository_path).is_absolute()
            || (observation.view_coverage != crate::core::GitViewCoverage::Checkout
                && observation
                    .repository_path
                    .contains(char::REPLACEMENT_CHARACTER))
        {
            return Ok(None);
        }
        let current = head.target_oid.as_str();
        let repository = observation.repository_id.to_string();
        let previous: Option<String> =
            if observation.view_coverage == crate::core::GitViewCoverage::Checkout {
                views.checkout_previous.clone()
            } else {
                conn.query_row(
                    "SELECT h.target_oid FROM git_refs h
             JOIN repositories r ON r.id = h.repository_id
             JOIN git_views v ON v.repository_id = h.repository_id AND v.commit_oid = h.target_oid
                             AND v.observed_at = h.last_observed_at
             WHERE h.repository_id = ?1 AND h.name = 'HEAD' AND h.target_kind = 'commit'
               AND r.current_path = ?2 AND h.target_oid <> ?3 AND h.last_observed_at <= ?4",
                    params![
                        repository,
                        observation.repository_path,
                        current,
                        observation.session.observed_at.0
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(dberr)?
            };
        let Some(previous) = previous else {
            return Ok(None);
        };

        if observation.view_coverage != crate::core::GitViewCoverage::Checkout
            && (memory_views.contains(&previous) || !memory_views.contains(&head.target_oid))
        {
            return Ok(None);
        }
        // Removing the old HEAD and adding this one must be the only view
        // changes. Unrelated worktrees that stayed at their tips coexist.
        let departing = views.before.difference(&views.after).map(String::as_str);
        let arriving = views.after.difference(&views.before).map(String::as_str);
        if !departing.eq([previous.as_str()]) || !arriving.eq([current]) {
            return Ok(None);
        }
        Ok(Some(Self { previous, current }))
    }

    pub(super) fn link_successors(
        &self,
        conn: &Connection,
        observation: &GitObservation,
        files: &mut [GitFileReport],
        deadline: &Deadline,
    ) -> Result<u64> {
        // Count the complete set of active episodes on both sides. Duplicate
        // normalized headings (including splits/merges) have no unique match.
        // SQLite otherwise starts with every active memory in the database.
        // Drive the indexed joins from the two repository-scoped commits.
        let mut stmt = conn
            .prepare(
                "SELECT max(m.id) FILTER (WHERE g.commit_oid = ?2) AS old_id,
                    max(m.id) FILTER (WHERE g.commit_oid = ?3) AS new_id
             FROM git_source_versions g
             CROSS JOIN evidence e ON e.source_id = g.source_id
             CROSS JOIN assertions a ON a.evidence_id = e.id
             CROSS JOIN memories m ON m.assertion_id = a.id
             WHERE g.repository_id = ?1 AND g.commit_oid IN (?2, ?3)
               AND g.path NOT LIKE '/refs/tags/%'
               AND m.state = 'active' AND m.producer_family = ?4
             GROUP BY g.path, a.subject_normalized
             HAVING count(*) FILTER (WHERE g.commit_oid = ?2) = 1
                AND count(*) FILTER (WHERE g.commit_oid = ?3) = 1",
            )
            .map_err(dberr)?;
        let rows = stmt
            .query_map(
                params![
                    observation.repository_id.to_string(),
                    self.previous,
                    self.current,
                    GIT_PRODUCER_FAMILY
                ],
                |row| Ok((uuid_col(row, "new_id")?, uuid_col(row, "old_id")?)),
            )
            .map_err(dberr)?;
        let mut predecessors = HashMap::new();
        for row in rows {
            deadline.check("matching Git HEAD episodes")?;
            let (new, old) = row.map_err(dberr)?;
            predecessors.insert(new, old);
        }

        let mut linked = 0;
        for file in files
            .iter_mut()
            .filter(|file| file.commit_oid == self.current)
        {
            for action in &mut file.actions {
                deadline.check("linking Git HEAD episodes")?;
                // A pre-existing historical episode may already coexist with
                // the outgoing episode. Only a fresh birth is its successor.
                if action.action == MemoryActionKind::Created {
                    if let Some(&old) = predecessors.get(&action.memory_id) {
                        supersede_existing_memory(
                            conn,
                            old,
                            action.memory_id,
                            observation.session.observed_at,
                        )?;
                        action.action = MemoryActionKind::Superseded { old_memory_id: old };
                        linked += 1;
                    }
                }
            }
        }
        Ok(linked)
    }
}
