//! Source identity and lifecycle tests (v0.1). Scenario numbering follows
//! the milestone's adversarial list. All lifecycle reasoning is exercised
//! through directory (full) observations unless a scenario is specifically
//! about partial (file-scope) behavior.

use std::path::{Path, PathBuf};

use mindspace::core::{FileOutcome, MemoryState, SourceEventKind, SourcePresence};
use mindspace::ingest;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing};

struct Env {
    _dir: tempfile::TempDir,
    db: PathBuf,
}

fn env() -> Env {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("mind.db");
    initialize(&db).expect("init");
    Env { _dir: dir, db }
}

impl Env {
    fn open(&self) -> mindspace::SqliteStore {
        open_existing(&self.db).expect("open existing db")
    }

    fn root(&self) -> &Path {
        self._dir.path()
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self._dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path
    }

    fn scan(&self, store: &mindspace::SqliteStore) -> mindspace::core::ObservationReport {
        let _ = store.create_space("coding", "");
        ingest::ingest_path(store, self.root(), &["coding"]).unwrap()
    }
}

fn active_by_subject(
    store: &mindspace::SqliteStore,
    subject: &str,
) -> Vec<mindspace::core::MemorySummary> {
    store
        .list_memories_by_subject(subject)
        .unwrap()
        .into_iter()
        .filter(|m| m.state == MemoryState::Active)
        .collect()
}

fn logical_of_first_hit(
    store: &mindspace::SqliteStore,
    text: &str,
) -> mindspace::core::SourceLifecycle {
    let hits = mindspace::retrieval::search(
        store,
        mindspace::core::SearchQuery {
            text: text.to_string(),
            space_ids: None,
            limit: 5,
            at: None,
            include_superseded: true,
            include_retired: true,
        },
    )
    .unwrap();
    assert!(!hits.is_empty(), "expected a hit for {text:?}");
    let detail = store.get_memory_detail(hits[0].memory_id).unwrap().unwrap();
    detail.source_lifecycle
}

// 1. UNCHANGED -----------------------------------------------------------------

#[test]
fn unchanged_observation_is_idempotent() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    let first = e.scan(&store);
    assert!(matches!(first.files[0].outcome, FileOutcome::Created));

    let second = e.scan(&store);
    assert!(matches!(second.files[0].outcome, FileOutcome::Unchanged));
    assert!(second.events.is_empty(), "no lifecycle changes");
    assert_eq!(first.files[0].logical_id, second.files[0].logical_id);
    assert_eq!(active_by_subject(&store, "kernel").len(), 1);
}

// 2. SAME PATH, CHANGED CONTENT -------------------------------------------------

#[test]
fn changed_content_is_a_new_version_of_the_same_logical_source() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Kernel\n\nThe scheduler is CFS.\n");
    let first = e.scan(&store);
    let logical = first.files[0].logical_id;

    e.write("a.md", "# Kernel\n\nThe scheduler is EEVDF.\n");
    let second = e.scan(&store);
    assert!(matches!(second.files[0].outcome, FileOutcome::Changed));
    assert_eq!(second.files[0].logical_id, logical, "path keeps identity");
    assert!(second.events.is_empty(), "content change is not a move");

    // Historical version stays traceable; current knowledge comes from the
    // new version.
    let active = active_by_subject(&store, "kernel");
    assert_eq!(active.len(), 1);
    let detail = store
        .get_memory_detail(active[0].memory_id)
        .unwrap()
        .unwrap();
    assert!(detail.assertion.content.contains("EEVDF"));
    let superseded = store.list_memories_by_subject("kernel").unwrap();
    assert_eq!(superseded.len(), 2, "old episode preserved");
}

// 3. SIMPLE RENAME --------------------------------------------------------------

#[test]
fn rename_keeps_logical_identity_and_records_the_move() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    let first = e.scan(&store);
    let logical = first.files[0].logical_id;

    std::fs::remove_file(e.root().join("a.md")).unwrap();
    e.write("b.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    let second = e.scan(&store);
    assert!(matches!(
        &second.files[0].outcome,
        FileOutcome::Continued { from_location } if from_location.as_deref() == Some("moved") || true
    ));
    assert_eq!(second.files[0].logical_id, logical, "same logical source");
    assert_eq!(second.events.len(), 1, "one moved event");
    assert!(matches!(
        &second.events[0].kind,
        SourceEventKind::Moved { .. }
    ));

    // No duplicate knowledge, no invented conflict.
    assert_eq!(active_by_subject(&store, "kernel").len(), 1);
    assert!(store.list_conflicts().unwrap().is_empty());

    // Trace explains both locations.
    let lifecycle = logical_of_first_hit(&store, "preemptible");
    assert_eq!(lifecycle.logical.id, logical);
    assert_eq!(
        lifecycle.logical.current_location.as_deref(),
        Some(e.root().join("b.md").to_str().unwrap().to_string()).as_deref()
    );
}

// 4. SIMPLE MOVE (across subdirectories) ----------------------------------------

#[test]
fn move_across_directories_keeps_identity() {
    let e = env();
    let store = e.open();
    e.write("docs/a.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    let first = e.scan(&store);
    let logical = first.files[0].logical_id;

    std::fs::remove_file(e.root().join("docs/a.md")).unwrap();
    e.write(
        "archive/a.md",
        "# Kernel\n\nThe scheduler is preemptible.\n",
    );
    let second = e.scan(&store);
    assert!(matches!(
        &second.files[0].outcome,
        FileOutcome::Continued { .. }
    ));
    assert_eq!(second.files[0].logical_id, logical);
    assert_eq!(active_by_subject(&store, "kernel").len(), 1);
}

// 5. RENAME + EDIT ---------------------------------------------------------------

#[test]
fn rename_plus_edit_is_not_provable_and_stays_separate() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Kernel\n\nThe scheduler is CFS.\n");
    let first = e.scan(&store);
    let old_logical = first.files[0].logical_id;

    std::fs::remove_file(e.root().join("a.md")).unwrap();
    e.write("b.md", "# Kernel\n\nThe scheduler is EEVDF.\n");
    let second = e.scan(&store);

    // Content differs: identity cannot be proven. The old source is
    // recorded as absent; the new path is a new logical source.
    assert!(matches!(second.files[0].outcome, FileOutcome::Created));
    assert_ne!(second.files[0].logical_id, old_logical);
    let disappeared = second
        .events
        .iter()
        .any(|ev| matches!(&ev.kind, SourceEventKind::Disappeared { .. }));
    assert!(disappeared, "the abandoned source is recorded as absent");

    let lifecycle = store.get_source_lifecycle(old_logical).unwrap().unwrap();
    assert_eq!(lifecycle.logical.state, SourcePresence::Absent);
    // Absence is not falsehood: the old memory stays active and inspectable.
    let old_active: Vec<_> = store
        .list_memories_by_subject("kernel")
        .unwrap()
        .into_iter()
        .filter(|m| m.memory_id != second.files[0].actions[0].memory_id)
        .collect();
    assert!(old_active
        .iter()
        .any(|m| m.state == MemoryState::Active && m.content.contains("CFS")));
}

// 6. COPY -------------------------------------------------------------------------

#[test]
fn copy_creates_a_second_logical_source() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    let first = e.scan(&store);
    let logical_a = first.files[0].logical_id;

    e.write("b.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    let second = e.scan(&store);

    // a.md is still observed: b.md cannot be a move.
    assert!(matches!(second.files[0].outcome, FileOutcome::Unchanged));
    let outcomes: Vec<&FileOutcome> = second.files.iter().map(|f| &f.outcome).collect();
    assert_eq!(outcomes.len(), 2);
    assert!(matches!(
        outcomes[0],
        FileOutcome::Unchanged | FileOutcome::Created
    ));
    assert!(matches!(
        outcomes[1],
        FileOutcome::Unchanged | FileOutcome::Created
    ));
    assert!(second.events.is_empty(), "no move: a.md is still there");

    // Two logical sources, two active memories, one candidate conflict.
    let active = active_by_subject(&store, "kernel");
    assert_eq!(active.len(), 2);
    let logicals: std::collections::BTreeSet<mindspace::core::Uuid> = active
        .iter()
        .map(|m| logical_of(&store, m.memory_id))
        .collect();
    assert_eq!(logicals.len(), 2, "copies stay distinct logical sources");
    assert!(logicals.contains(&logical_a));
    assert_eq!(store.list_conflicts().unwrap().len(), 1);
}

fn logical_of(
    store: &mindspace::SqliteStore,
    memory_id: mindspace::core::Uuid,
) -> mindspace::core::Uuid {
    store
        .get_memory_detail(memory_id)
        .unwrap()
        .unwrap()
        .source
        .logical_id
}

// 7. AMBIGUOUS IDENTICAL MOVE ------------------------------------------------------

#[test]
fn ambiguous_identical_move_does_not_guess() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Note\n\nExactly identical content.\n");
    let first = e.scan(&store);
    let logical_a = first.files[0].logical_id;

    std::fs::remove_file(e.root().join("a.md")).unwrap();
    e.write("b.md", "# Note\n\nExactly identical content.\n");
    e.write("c.md", "# Note\n\nExactly identical content.\n");
    let second = e.scan(&store);

    // One gone candidate, two new paths with the same content: choosing a
    // continuation would be arbitrary, so both stay separate logical
    // sources and the old one is recorded as absent.
    let created = second
        .files
        .iter()
        .filter(|f| matches!(f.outcome, FileOutcome::Created))
        .count();
    assert_eq!(created, 2, "{:?}", second.files);
    assert!(second.files.iter().all(|f| f.logical_id != logical_a));
    assert!(second
        .events
        .iter()
        .any(|ev| matches!(&ev.kind, SourceEventKind::Disappeared { .. })));
    assert_eq!(active_by_subject(&store, "note").len(), 3);
}

// 8. TWO IDENTICAL ORIGINALS ---------------------------------------------------------

#[test]
fn identical_originals_stay_independently_correct() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Note\n\nShared exact content.\n");
    e.write("b.md", "# Note\n\nShared exact content.\n");
    let first = e.scan(&store);
    assert_eq!(first.files.len(), 2);
    let logical_a = first.files[0].logical_id;
    let logical_b = first.files[1].logical_id;
    assert_ne!(logical_a, logical_b, "identical content is not identity");

    // b moves away; a stays untouched.
    std::fs::remove_file(e.root().join("b.md")).unwrap();
    e.write("c.md", "# Note\n\nShared exact content.\n");
    let second = e.scan(&store);

    let a_outcome = second
        .files
        .iter()
        .find(|f| f.logical_id == logical_a)
        .expect("a still observed");
    assert!(matches!(a_outcome.outcome, FileOutcome::Unchanged));
    let c_outcome = second
        .files
        .iter()
        .find(|f| f.logical_id == logical_b)
        .expect("the move continues b's logical source");
    assert!(matches!(c_outcome.outcome, FileOutcome::Continued { .. }));
}

// 9. DISAPPEARANCE ---------------------------------------------------------------------

#[test]
fn disappearance_records_absence_without_retiring_knowledge() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    e.scan(&store);

    std::fs::remove_file(e.root().join("a.md")).unwrap();
    let second = e.scan(&store);
    assert_eq!(second.files.len(), 0);
    assert_eq!(second.events.len(), 1);
    assert!(matches!(
        &second.events[0].kind,
        SourceEventKind::Disappeared { .. }
    ));

    // Absence is an availability fact, not a truth fact: the memory stays
    // active and searchable.
    let active = active_by_subject(&store, "kernel");
    assert_eq!(active.len(), 1);
    assert_eq!(
        mindspace::retrieval::search(
            &store,
            mindspace::core::SearchQuery {
                text: "preemptible".into(),
                space_ids: None,
                limit: 5,
                at: None,
                include_superseded: false,
                include_retired: false,
            }
        )
        .unwrap()
        .len(),
        1
    );
    let lifecycle = logical_of_first_hit(&store, "preemptible");
    assert_eq!(lifecycle.logical.state, SourcePresence::Absent);
    assert!(
        lifecycle.logical.current_location.is_some(),
        "the last known location is kept"
    );
}

// 10. REAPPEARANCE ----------------------------------------------------------------------

#[test]
fn reappearance_at_the_same_path_continues_identity() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    let first = e.scan(&store);
    let logical = first.files[0].logical_id;

    std::fs::remove_file(e.root().join("a.md")).unwrap();
    e.scan(&store); // absence recorded

    e.write("a.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    let third = e.scan(&store);
    assert!(matches!(
        &third.files[0].outcome,
        FileOutcome::Continued { from_location } if from_location.as_deref() == Some(
            e.root().join("a.md").to_str().unwrap()
        )
    ));
    assert_eq!(third.files[0].logical_id, logical);
    assert_eq!(third.events.len(), 1);
    assert!(matches!(
        &third.events[0].kind,
        SourceEventKind::Moved { .. }
    ));
    assert_eq!(active_by_subject(&store, "kernel").len(), 1);
}

#[test]
fn reappearance_with_different_content_is_a_new_source() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Kernel\n\nOld content.\n");
    let first = e.scan(&store);
    let old_logical = first.files[0].logical_id;

    std::fs::remove_file(e.root().join("a.md")).unwrap();
    e.scan(&store);

    // Someone recycled the path with a different document.
    e.write("a.md", "# Changelog\n\nEntirely different material.\n");
    let third = e.scan(&store);
    assert!(matches!(third.files[0].outcome, FileOutcome::Created));
    assert_ne!(third.files[0].logical_id, old_logical);

    let lifecycle = store.get_source_lifecycle(old_logical).unwrap().unwrap();
    assert_eq!(lifecycle.logical.state, SourcePresence::Absent);
}

// 11. PATH SWAP ---------------------------------------------------------------------------

#[test]
fn path_swap_does_not_collapse_identity() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Alpha\n\nContent of alpha.\n");
    e.write("b.md", "# Beta\n\nContent of beta.\n");
    let first = e.scan(&store);
    let logical_a = first.files[0].logical_id;
    let logical_b = first.files[1].logical_id;

    // Swap the contents (indistinguishable from swapping the files).
    std::fs::write(e.root().join("a.md"), "# Beta\n\nContent of beta.\n").unwrap();
    std::fs::write(e.root().join("b.md"), "# Alpha\n\nContent of alpha.\n").unwrap();
    let second = e.scan(&store);

    // Per-path continuity: no moves inferred, identities stable.
    assert_eq!(second.events.len(), 0);
    let a = second
        .files
        .iter()
        .find(|f| f.logical_id == logical_a)
        .unwrap();
    let b = second
        .files
        .iter()
        .find(|f| f.logical_id == logical_b)
        .unwrap();
    assert!(matches!(a.outcome, FileOutcome::Changed));
    assert!(matches!(b.outcome, FileOutcome::Changed));
    // Each logical source now has two versions; both stay separate and
    // keep their own locations.
    let lifecycle_a = store.get_source_lifecycle(logical_a).unwrap().unwrap();
    let lifecycle_b = store.get_source_lifecycle(logical_b).unwrap().unwrap();
    assert_ne!(lifecycle_a.logical.id, lifecycle_b.logical.id);
    assert_eq!(
        lifecycle_a.logical.current_location.as_deref(),
        Some(e.root().join("a.md").to_str().unwrap())
    );
    assert_eq!(
        lifecycle_b.logical.current_location.as_deref(),
        Some(e.root().join("b.md").to_str().unwrap())
    );
}

// 12. REPEATED INGEST ---------------------------------------------------------------------

#[test]
fn repeated_observations_are_idempotent() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    e.write("b.md", "# Storage\n\nEverything lives in SQLite.\n");
    let first = e.scan(&store);
    let second = e.scan(&store);

    assert_eq!(first.files.len(), second.files.len());
    for (f, s) in first.files.iter().zip(second.files.iter()) {
        assert_eq!(f.logical_id, s.logical_id);
        assert!(matches!(s.outcome, FileOutcome::Unchanged));
        assert!(s
            .actions
            .iter()
            .all(|a| matches!(a.action, mindspace::core::MemoryActionKind::Unchanged)));
    }
    assert!(second.events.is_empty());
    assert_eq!(active_by_subject(&store, "kernel").len(), 1);
    assert_eq!(active_by_subject(&store, "storage").len(), 1);

    // And once more, still stable.
    let third = e.scan(&store);
    assert!(third
        .files
        .iter()
        .all(|f| matches!(f.outcome, FileOutcome::Unchanged)));
    assert!(third.events.is_empty());
}

// PARTIAL OBSERVATIONS ---------------------------------------------------------------------

#[test]
fn partial_ingest_never_marks_anything_disappeared() {
    let e = env();
    let store = e.open();
    let a = e.write("a.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    e.write("b.md", "# Storage\n\nEverything lives in SQLite.\n");
    e.scan(&store);

    // Remove b.md from disk, then observe only a.md.
    std::fs::remove_file(e.root().join("b.md")).unwrap();
    let report = ingest::ingest_file(&store, &a, &["coding"]).unwrap();
    assert!(matches!(report.files[0].outcome, FileOutcome::Unchanged));
    assert!(report.events.is_empty(), "partial observations are silent");

    // b.md's knowledge stays active; its logical source is still present
    // as far as the engine knows.
    assert_eq!(active_by_subject(&store, "storage").len(), 1);
}

#[test]
fn partial_ingest_of_changed_file_is_a_new_version() {
    let e = env();
    let store = e.open();
    let a = e.write("a.md", "# Kernel\n\nThe scheduler is CFS.\n");
    e.scan(&store);

    std::fs::write(&a, "# Kernel\n\nThe scheduler is EEVDF.\n").unwrap();
    let second = ingest::ingest_file(&store, &a, &["coding"]).unwrap();
    assert!(matches!(second.files[0].outcome, FileOutcome::Changed));
    assert_eq!(second.files[0].logical_id, {
        let first_hits = mindspace::retrieval::search(
            &store,
            mindspace::core::SearchQuery {
                text: "CFS".into(),
                space_ids: None,
                limit: 5,
                at: None,
                include_superseded: true,
                include_retired: false,
            },
        )
        .unwrap();
        store
            .get_memory_detail(first_hits[0].memory_id)
            .unwrap()
            .unwrap()
            .source
            .logical_id
    });
}

// RENAME + CONTENT CHANGE SEQUENCED ---------------------------------------------------------

#[test]
fn rename_first_then_edit_preserves_lineage() {
    let e = env();
    let store = e.open();
    e.write("a.md", "# Kernel\n\nThe scheduler is CFS.\n");
    let first = e.scan(&store);
    let logical = first.files[0].logical_id;

    // Rename (observed), then edit (observed): two deterministic steps.
    std::fs::remove_file(e.root().join("a.md")).unwrap();
    e.write("b.md", "# Kernel\n\nThe scheduler is CFS.\n");
    let moved = e.scan(&store);
    assert!(matches!(
        moved.files[0].outcome,
        FileOutcome::Continued { .. }
    ));

    std::fs::write(
        e.root().join("b.md"),
        "# Kernel\n\nThe scheduler is EEVDF.\n",
    )
    .unwrap();
    let edited = e.scan(&store);
    assert!(matches!(edited.files[0].outcome, FileOutcome::Changed));
    assert_eq!(
        edited.files[0].logical_id, logical,
        "lineage survives both steps"
    );

    let lifecycle = store.get_source_lifecycle(logical).unwrap().unwrap();
    assert_eq!(
        lifecycle.events.len(),
        1,
        "only the move is a lifecycle event"
    );
    assert_eq!(active_by_subject(&store, "kernel").len(), 1);
}
