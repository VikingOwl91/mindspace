//! Retrieval behavior, offline and deterministic.
//!
//! Semantic paths run against a synthetic model: its artifacts are a few bytes
//! and its runtime is a fake. No test downloads or loads the real model.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use mindspace::core::{
    AssertionEmbedding, Error, ManualMemoryDraft, ManualMemoryReport, SearchQuery, Ts, Uuid,
};
use mindspace::embeddings::manager;
use mindspace::embeddings::manifest::{derivation_from_manifest, Artifact, ModelManifestEntry};
use mindspace::embeddings::EmbeddingRuntime;
use mindspace::retrieval::fusion::rrf_fuse;
use mindspace::retrieval::semantic::resolve_with;
use mindspace::retrieval::{
    self, candidate_pool_size, EffectiveMode, FallbackReason, RetrievalOptions, ScoreKind,
    SearchMode, SearchOutcome, SemanticCapability, RRF_K_DEFAULT,
};
use mindspace::storage::Store;
use mindspace::SqliteStore;
use rusqlite::OpenFlags;
use tempfile::TempDir;

const FAKE_ID: &str = "test-embed-model";
const FAKE_DIM: usize = 4;
const MODEL_META_KEY: &str = "embedding.model";
const MODEL_BYTES: &[u8] = b"synthetic model artifact for retrieval tests\n";
const MODEL_SHA256: &str = "269cbb1a5ed49786cc52cfa6e29a80d5147d461ca878f9c84651c947bf5f750f";
const TOKENIZER_BYTES: &[u8] = b"{\"synthetic\":true}\n";
const TOKENIZER_SHA256: &str = "cb8daed7b30399a1c3c8b83b3b7bee4b774a30fc389afc1d375e4c23a3cc4ae8";

static FAKE_ENTRY: ModelManifestEntry = ModelManifestEntry {
    id: FAKE_ID,
    version: "test-1",
    revision: "test",
    architecture: "synthetic",
    runtime: "fake",
    runtime_version: "0",
    dimension: FAKE_DIM as u32,
    max_seq_len: 512,
    truncation: "none",
    prefix_document: "search_document: ",
    prefix_query: "search_query: ",
    pooling: "mean",
    normalization: "l2",
    license: "test-only",
    source: "synthetic",
    artifacts: &[
        Artifact {
            filename: "model.safetensors",
            url: "https://example.invalid/model.safetensors",
            sha256: MODEL_SHA256,
            size: MODEL_BYTES.len() as u64,
        },
        Artifact {
            filename: "tokenizer.json",
            url: "https://example.invalid/tokenizer.json",
            sha256: TOKENIZER_SHA256,
            size: TOKENIZER_BYTES.len() as u64,
        },
    ],
};

/// A filter configuration and the memories it must return:
/// (label, spaces, include superseded, include retired, expected memories).
type FilterCase = (&'static str, Option<Vec<Uuid>>, bool, bool, Vec<Uuid>);

fn lookup(id: &str) -> Option<&'static ModelManifestEntry> {
    (id == FAKE_ID).then_some(&FAKE_ENTRY)
}

fn fake_key() -> String {
    derivation_from_manifest(&FAKE_ENTRY).derivation_key()
}

/// The same pipeline with a different artifact digest, so a different key.
fn alternate_key() -> String {
    let mut derivation = derivation_from_manifest(&FAKE_ENTRY);
    derivation.model_sha256 = "0".repeat(64);
    derivation.derivation_key()
}

/// Row counts for every table and every metadata entry. Any write changes it.
fn snapshot_db(path: &Path) -> Vec<String> {
    let conn =
        rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let tables: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        rows.map(|row| row.unwrap()).collect()
    };
    let mut snapshot = Vec::new();
    for table in tables {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
                row.get(0)
            })
            .unwrap();
        snapshot.push(format!("{table}={count}"));
    }
    let mut stmt = conn
        .prepare("SELECT key || '=' || value FROM mind_meta ORDER BY key")
        .unwrap();
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
    snapshot.extend(rows.map(|row| row.unwrap()));
    snapshot
}

/// Every path under `root` with its size, sorted. Empty when `root` is absent.
fn snapshot_tree(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            out.push(format!("{} {}", path.display(), meta.len()));
            if meta.is_dir() {
                walk(&path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

fn write_artifacts(base: &Path, model: &[u8], tokenizer: &[u8]) {
    let dir = manager::model_dir_in(base, FAKE_ID);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("model.safetensors"), model).unwrap();
    fs::write(dir.join("tokenizer.json"), tokenizer).unwrap();
}

/// Counters shared by the fake runtime and the factory that builds it.
#[derive(Default)]
struct Calls {
    factory: AtomicUsize,
    queries: AtomicUsize,
    documents: AtomicUsize,
    query_texts: Mutex<Vec<String>>,
}

struct FakeRuntime {
    /// `None` makes every query embedding fail.
    answer: Option<Vec<f32>>,
    calls: Arc<Calls>,
}

impl EmbeddingRuntime for FakeRuntime {
    fn embed_document(&self, _text: &str) -> mindspace::Result<Vec<f32>> {
        self.calls.documents.fetch_add(1, Ordering::SeqCst);
        Err(Error::Storage(
            "search must not embed documents".to_string(),
        ))
    }

    fn embed_query(&self, text: &str) -> mindspace::Result<Vec<f32>> {
        self.calls.queries.fetch_add(1, Ordering::SeqCst);
        self.calls
            .query_texts
            .lock()
            .unwrap()
            .push(text.to_string());
        self.answer
            .clone()
            .ok_or_else(|| Error::Storage("fake query failure".to_string()))
    }

    fn dimension(&self) -> usize {
        FAKE_DIM
    }

    fn model_id(&self) -> &str {
        FAKE_ID
    }
}

struct Fixture {
    _dir: TempDir,
    db: PathBuf,
    base: PathBuf,
    store: SqliteStore,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mind.db");
        SqliteStore::open(&db).unwrap();
        let store = SqliteStore::open(&db).unwrap();
        let base = dir.path().join("data");
        Self {
            _dir: dir,
            db,
            base,
            store,
        }
    }

    /// Enables the synthetic model and writes artifacts that pass every check.
    fn enable_with_files(&self) {
        manager::set_enabled(&self.store, true).unwrap();
        self.store.set_meta(MODEL_META_KEY, FAKE_ID).unwrap();
        write_artifacts(&self.base, MODEL_BYTES, TOKENIZER_BYTES);
    }

    /// Resolves the capability. The fake runtime answers queries with `answer`.
    fn resolve(&self, answer: Option<Vec<f32>>, calls: &Arc<Calls>) -> SemanticCapability {
        let factory =
            |_: &ModelManifestEntry, _: &Path| -> mindspace::Result<Box<dyn EmbeddingRuntime>> {
                calls.factory.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(FakeRuntime {
                    answer: answer.clone(),
                    calls: Arc::clone(calls),
                }))
            };
        resolve_with(&self.store, &self.base, lookup, &factory).unwrap()
    }
}

fn space(store: &SqliteStore, name: &str) -> Uuid {
    match store.create_space(name, "") {
        Ok(space) => space.id,
        Err(_) => store.get_space(name).unwrap().id,
    }
}

fn write(
    store: &SqliteStore,
    subject: &str,
    content: &str,
    spaces: &[Uuid],
    supersedes: Option<Uuid>,
) -> ManualMemoryReport {
    store
        .record_manual_write(ManualMemoryDraft {
            subject: Some(subject.to_string()),
            content: content.to_string(),
            spaces: spaces.to_vec(),
            supersedes,
        })
        .unwrap()
}

fn index(store: &SqliteStore, key: &str, assertion_id: Uuid, vector: &[f32]) {
    store
        .save_embeddings(&[AssertionEmbedding {
            assertion_id,
            derivation_key: key.to_string(),
            model: FAKE_ID.to_string(),
            model_version: FAKE_ENTRY.version.to_string(),
            dimension: vector.len() as u32,
            embedding: vector.to_vec(),
        }])
        .unwrap();
}

fn query(text: &str, spaces: Option<Vec<Uuid>>, limit: u32) -> SearchQuery {
    SearchQuery {
        text: text.to_string(),
        space_ids: spaces,
        limit,
        at: None,
        include_superseded: false,
        include_retired: false,
    }
}

fn run(
    store: &SqliteStore,
    query: SearchQuery,
    mode: SearchMode,
    capability: &SemanticCapability,
) -> mindspace::Result<SearchOutcome> {
    retrieval::search_with_mode(store, query, mode, capability, RetrievalOptions::default())
}

fn ids(outcome: &SearchOutcome) -> Vec<Uuid> {
    outcome.hits.iter().map(|hit| hit.hit.memory_id).collect()
}

fn id_set(outcome: &SearchOutcome) -> BTreeSet<Uuid> {
    ids(outcome).into_iter().collect()
}

fn reason_of(capability: &SemanticCapability) -> Option<&FallbackReason> {
    match capability {
        SemanticCapability::Unavailable(reason) => Some(reason),
        SemanticCapability::Ready(_) => None,
    }
}

fn unavailable(reason: FallbackReason) -> SemanticCapability {
    SemanticCapability::Unavailable(reason)
}

fn assert_ready(capability: &SemanticCapability) {
    assert!(
        matches!(capability, SemanticCapability::Ready(_)),
        "expected a ready capability, got {capability:?}"
    );
}

/// Four memories whose rankings disagree. "alpha" matches p and q only. The
/// query vector [1, 0, 0, 0] ranks them s, q, r, p by cosine similarity.
fn seed_disagreeing_rankings(f: &Fixture) -> [Uuid; 4] {
    let coding = space(&f.store, "coding");
    let key = fake_key();
    let p = write(&f.store, "p", "shared alpha beta", &[coding], None);
    let q = write(&f.store, "q", "shared alpha", &[coding], None);
    let r = write(&f.store, "r", "shared beta", &[coding], None);
    let s = write(&f.store, "s", "unrelated gamma", &[coding], None);
    index(&f.store, &key, p.assertion_id, &[0.2, 0.9, 0.0, 0.0]);
    index(&f.store, &key, q.assertion_id, &[0.9, 0.1, 0.0, 0.0]);
    index(&f.store, &key, r.assertion_id, &[0.6, 0.6, 0.0, 0.0]);
    index(&f.store, &key, s.assertion_id, &[1.0, 0.0, 0.0, 0.0]);
    [p.memory_id, q.memory_id, r.memory_id, s.memory_id]
}

#[test]
fn lexical_mode_runs_without_embeddings_and_matches_legacy_search() {
    let f = Fixture::new();
    let coding = space(&f.store, "coding");
    write(&f.store, "alpha", "retrieval alpha note", &[coding], None);
    write(&f.store, "beta", "retrieval beta note", &[coding], None);

    let legacy = retrieval::search(&f.store, query("retrieval", None, 10)).unwrap();
    let capability = unavailable(FallbackReason::EmbeddingsDisabled);
    let outcome = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Lexical,
        &capability,
    )
    .unwrap();

    assert_eq!(SearchMode::default(), SearchMode::Lexical);
    assert_eq!(outcome.requested, SearchMode::Lexical);
    assert_eq!(outcome.effective, EffectiveMode::Lexical);
    assert_eq!(outcome.fallback, None);
    assert_eq!(outcome.semantic_candidates, 0);
    assert_eq!(outcome.hits.len(), 2);
    for (retrieved, legacy_hit) in outcome.hits.iter().zip(&legacy) {
        assert_eq!(&retrieved.hit, legacy_hit);
        assert_eq!(retrieved.score_kind, ScoreKind::Lexical);
        assert_eq!(retrieved.provenance.semantic_rank, None);
    }
    assert_eq!(outcome.hits[0].provenance.lexical_rank, Some(1));
}

#[test]
fn semantic_queries_go_through_embed_query_with_trimmed_text() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let note = write(&f.store, "ownership", "rust borrow rules", &[coding], None);
    index(
        &f.store,
        &fake_key(),
        note.assertion_id,
        &[1.0, 0.0, 0.0, 0.0],
    );

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert_ready(&capability);

    let semantic = run(
        &f.store,
        query("   rust ownership  ", None, 5),
        SearchMode::Semantic,
        &capability,
    )
    .unwrap();
    assert_eq!(semantic.effective, EffectiveMode::Semantic);
    assert_eq!(ids(&semantic), vec![note.memory_id]);

    let hybrid = run(
        &f.store,
        query("   rust ownership  ", None, 5),
        SearchMode::Hybrid,
        &capability,
    )
    .unwrap();
    assert_eq!(hybrid.effective, EffectiveMode::Hybrid);

    assert_eq!(calls.queries.load(Ordering::SeqCst), 2);
    assert_eq!(
        calls.documents.load(Ordering::SeqCst),
        0,
        "search must never embed documents"
    );
    assert_eq!(
        *calls.query_texts.lock().unwrap(),
        vec!["rust ownership".to_string(), "rust ownership".to_string()]
    );
}

#[test]
fn semantic_matches_only_the_active_derivation_key() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let active = write(
        &f.store,
        "active",
        "derivation probe active",
        &[coding],
        None,
    );
    let other = write(&f.store, "other", "derivation probe other", &[coding], None);
    // Identical vectors: only the derivation key separates the two memories.
    index(
        &f.store,
        &fake_key(),
        active.assertion_id,
        &[1.0, 0.0, 0.0, 0.0],
    );
    index(
        &f.store,
        &alternate_key(),
        other.assertion_id,
        &[1.0, 0.0, 0.0, 0.0],
    );
    assert_ne!(fake_key(), alternate_key());

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    let SemanticCapability::Ready(source) = &capability else {
        panic!("expected a ready capability, got {capability:?}");
    };
    assert_eq!(source.derivation_key(), fake_key());

    let outcome = run(
        &f.store,
        query("derivation", None, 10),
        SearchMode::Semantic,
        &capability,
    )
    .unwrap();
    assert_eq!(ids(&outcome), vec![active.memory_id]);
}

#[test]
fn semantic_ranks_by_cosine_and_truncates_to_limit() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let probes: [(&str, [f32; 4]); 5] = [
        ("exact", [1.0, 0.0, 0.0, 0.0]),
        ("steep", [2.0, 1.0, 0.0, 0.0]),
        ("diagonal", [1.0, 1.0, 0.0, 0.0]),
        ("shallow", [1.0, 2.0, 0.0, 0.0]),
        ("orthogonal", [0.0, 1.0, 0.0, 0.0]),
    ];
    let mut written: Vec<(&str, Uuid)> = Vec::new();
    for (name, vector) in probes {
        let report = write(
            &f.store,
            name,
            &format!("ranking probe {name}"),
            &[coding],
            None,
        );
        index(&f.store, &fake_key(), report.assertion_id, &vector);
        written.push((name, report.memory_id));
    }
    let id_of = |name: &str| written.iter().find(|(n, _)| *n == name).unwrap().1;

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);

    let top3 = run(
        &f.store,
        query("ranking probe", None, 3),
        SearchMode::Semantic,
        &capability,
    )
    .unwrap();
    assert_eq!(
        ids(&top3),
        vec![id_of("exact"), id_of("steep"), id_of("diagonal")]
    );
    let expected_scores = [1.0, 2.0 / 5f64.sqrt(), 1.0 / 2f64.sqrt()];
    for (hit, expected) in top3.hits.iter().zip(expected_scores) {
        assert_eq!(hit.score_kind, ScoreKind::Semantic);
        assert!(
            (hit.hit.score - expected).abs() < 1e-6,
            "{} vs {expected}",
            hit.hit.score
        );
    }
    let ranks: Vec<Option<u32>> = top3
        .hits
        .iter()
        .map(|hit| hit.provenance.semantic_rank)
        .collect();
    assert_eq!(ranks, vec![Some(1), Some(2), Some(3)]);
    assert!(top3
        .hits
        .iter()
        .all(|hit| hit.provenance.lexical_rank.is_none()));
    assert_eq!(top3.semantic_candidates, 3);

    let all = run(
        &f.store,
        query("ranking probe", None, 10),
        SearchMode::Semantic,
        &capability,
    )
    .unwrap();
    assert_eq!(
        ids(&all),
        vec![
            id_of("exact"),
            id_of("steep"),
            id_of("diagonal"),
            id_of("shallow"),
            id_of("orthogonal"),
        ]
    );
    assert_eq!(all.semantic_candidates, 5);
}

#[test]
fn hybrid_order_is_rrf_over_the_branch_candidate_lists() {
    let f = Fixture::new();
    f.enable_with_files();
    seed_disagreeing_rankings(&f);
    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);

    let limit = 3;
    let pool = candidate_pool_size(limit, None);
    let lexical = f.store.search(&query("alpha", None, pool)).unwrap();
    let semantic = f
        .store
        .search_vectors(
            &[1.0, 0.0, 0.0, 0.0],
            &query("alpha", None, pool),
            &fake_key(),
        )
        .unwrap();
    let lexical_ids: Vec<Uuid> = lexical.iter().map(|hit| hit.assertion_id).collect();
    let semantic_ids: Vec<Uuid> = semantic.iter().map(|hit| hit.assertion_id).collect();
    let mut representative_by_assertion: BTreeMap<Uuid, Uuid> = semantic
        .iter()
        .map(|hit| (hit.assertion_id, hit.memory_id))
        .collect();
    representative_by_assertion.extend(lexical.iter().map(|hit| (hit.assertion_id, hit.memory_id)));
    let fused = rrf_fuse(&lexical_ids, &semantic_ids, RRF_K_DEFAULT, limit as usize);
    let expected_memory_ids: Vec<Uuid> = fused
        .iter()
        .map(|entry| representative_by_assertion[&entry.assertion_id])
        .collect();

    let outcome = run(
        &f.store,
        query("alpha", None, limit),
        SearchMode::Hybrid,
        &capability,
    )
    .unwrap();
    assert_eq!(outcome.effective, EffectiveMode::Hybrid);
    assert_eq!(fused.len(), 3);
    assert_eq!(ids(&outcome), expected_memory_ids);
    for (hit, expected) in outcome.hits.iter().zip(&fused) {
        assert!((hit.hit.score - expected.score).abs() < 1e-12);
        assert_eq!(hit.provenance.lexical_rank, expected.lexical_rank);
        assert_eq!(hit.provenance.semantic_rank, expected.semantic_rank);
    }
    assert_eq!(outcome.lexical_candidates, 2);
    assert_eq!(outcome.semantic_candidates, 4);
}

#[test]
fn hybrid_fuses_semantic_candidates_when_lexical_branch_is_empty() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let key = fake_key();
    let candidates = [
        ("closest", [1.0, 0.0, 0.0, 0.0], 1.0),
        ("middle", [0.8, 0.6, 0.0, 0.0], 0.8),
        ("orthogonal", [0.0, 1.0, 0.0, 0.0], 0.0),
    ];
    let mut expected = Vec::new();
    for (name, vector, similarity) in candidates {
        let report = write(
            &f.store,
            name,
            &format!("semantic-only candidate {name}"),
            &[coding],
            None,
        );
        index(&f.store, &key, report.assertion_id, &vector);
        expected.push((report.memory_id, similarity));
    }

    let text = "unmatchedlexicaltoken";
    assert!(
        f.store.search(&query(text, None, 20)).unwrap().is_empty(),
        "the lexical branch must have no candidates"
    );
    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);

    let outcome = run(
        &f.store,
        query(text, None, 3),
        SearchMode::Hybrid,
        &capability,
    )
    .unwrap();

    assert_eq!(outcome.effective, EffectiveMode::Hybrid);
    assert_eq!(outcome.fallback, None);
    assert_eq!(outcome.lexical_candidates, 0);
    assert_eq!(outcome.semantic_candidates, 3);
    assert_eq!(outcome.hits.len(), expected.len());
    for (index, (hit, (expected_id, similarity))) in outcome.hits.iter().zip(&expected).enumerate()
    {
        let rank = (index + 1) as u32;
        assert_eq!(hit.hit.memory_id, *expected_id);
        assert_eq!(hit.score_kind, ScoreKind::Fused);
        assert_eq!(hit.provenance.lexical_rank, None);
        assert_eq!(hit.provenance.semantic_rank, Some(rank));
        assert_eq!(hit.provenance.lexical_score, None);
        let semantic_score = hit
            .provenance
            .semantic_score
            .expect("candidate came from the semantic branch");
        assert!((semantic_score - similarity).abs() < 1e-6);
        let expected_rrf = 1.0 / (f64::from(RRF_K_DEFAULT.get()) + f64::from(rank));
        assert!((hit.hit.score - expected_rrf).abs() < 1e-12);
    }
}

#[test]
fn hybrid_deduplicates_historical_episodes_by_assertion_after_filtering() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let linux = space(&f.store, "linux");
    let key = fake_key();

    let old = write(
        &f.store,
        "alpha",
        "assertion episode probe",
        &[coding],
        None,
    );
    f.store
        .retire_memory(old.memory_id, None, Ts::now())
        .unwrap();
    let current = write(&f.store, "alpha", "assertion episode probe", &[linux], None);
    assert_eq!(old.assertion_id, current.assertion_id);
    assert_ne!(old.memory_id, current.memory_id);
    index(&f.store, &key, current.assertion_id, &[1.0, 0.0, 0.0, 0.0]);

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);

    let mut all_episodes = query("assertion episode", None, 10);
    all_episodes.include_retired = true;
    let legacy = retrieval::search(&f.store, all_episodes.clone()).unwrap();
    assert_eq!(
        legacy
            .iter()
            .map(|hit| hit.memory_id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([old.memory_id, current.memory_id]),
        "legacy lexical search remains episode-level"
    );
    let semantic = run(
        &f.store,
        all_episodes.clone(),
        SearchMode::Semantic,
        &capability,
    )
    .unwrap();
    assert_eq!(
        id_set(&semantic),
        BTreeSet::from([old.memory_id, current.memory_id]),
        "semantic search remains episode-level"
    );

    for mode in [SearchMode::Hybrid, SearchMode::Auto] {
        let outcome = run(&f.store, all_episodes.clone(), mode, &capability).unwrap();
        assert_eq!(outcome.effective, EffectiveMode::Hybrid);
        assert_eq!(outcome.lexical_candidates, 1);
        assert_eq!(outcome.semantic_candidates, 1);
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].hit.memory_id, old.memory_id);
        assert_eq!(outcome.hits[0].provenance.lexical_rank, Some(1));
        assert_eq!(outcome.hits[0].provenance.semantic_rank, Some(1));
    }

    // Filter each episode separately. The ineligible episode must not be
    // substituted just because it shares the assertion identity.
    let old_only = run(
        &f.store,
        {
            let mut q = query("assertion episode", Some(vec![coding]), 10);
            q.include_retired = true;
            q
        },
        SearchMode::Hybrid,
        &capability,
    )
    .unwrap();
    assert_eq!(ids(&old_only), vec![old.memory_id]);
    let current_only = run(
        &f.store,
        query("assertion episode", Some(vec![linux]), 10),
        SearchMode::Hybrid,
        &capability,
    )
    .unwrap();
    assert_eq!(ids(&current_only), vec![current.memory_id]);
}

#[test]
fn memory_found_by_both_branches_is_fused_once() {
    let f = Fixture::new();
    f.enable_with_files();
    let [p, _, _, _] = seed_disagreeing_rankings(&f);
    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);

    let outcome = run(
        &f.store,
        query("alpha", None, 10),
        SearchMode::Hybrid,
        &capability,
    )
    .unwrap();
    let unique: BTreeSet<Uuid> = ids(&outcome).into_iter().collect();
    assert_eq!(
        unique.len(),
        outcome.hits.len(),
        "no memory may appear twice"
    );

    let hit = outcome
        .hits
        .iter()
        .find(|hit| hit.hit.memory_id == p)
        .expect("p is returned by both branches");
    assert_eq!(hit.score_kind, ScoreKind::Fused);
    let lexical_rank = hit
        .provenance
        .lexical_rank
        .expect("p is a lexical candidate");
    let semantic_rank = hit
        .provenance
        .semantic_rank
        .expect("p is a semantic candidate");
    let k = f64::from(RRF_K_DEFAULT.get());
    let expected = 1.0 / (k + f64::from(lexical_rank)) + 1.0 / (k + f64::from(semantic_rank));
    assert!(
        (hit.hit.score - expected).abs() < 1e-12,
        "{} vs {expected}",
        hit.hit.score
    );
    assert!(hit.provenance.lexical_score.is_some());
    assert!(hit.provenance.semantic_score.is_some());
}

#[test]
fn semantic_ties_break_by_memory_id_and_hybrid_is_deterministic() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let mut memories = Vec::new();
    for name in ["tie one", "tie two", "tie three"] {
        let report = write(
            &f.store,
            name,
            &format!("tie probe {name}"),
            &[coding],
            None,
        );
        // Identical vectors give identical cosine scores.
        index(
            &f.store,
            &fake_key(),
            report.assertion_id,
            &[1.0, 0.0, 0.0, 0.0],
        );
        memories.push(report.memory_id);
    }
    memories.sort();

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    for _ in 0..5 {
        let semantic = run(
            &f.store,
            query("tie probe", None, 10),
            SearchMode::Semantic,
            &capability,
        )
        .unwrap();
        assert_eq!(
            ids(&semantic),
            memories,
            "equal scores fall back to memory id order"
        );
        let scores: Vec<f64> = semantic.hits.iter().map(|hit| hit.hit.score).collect();
        assert!(scores.windows(2).all(|pair| pair[0] == pair[1]));
    }

    let baseline = run(
        &f.store,
        query("tie probe", None, 10),
        SearchMode::Hybrid,
        &capability,
    )
    .unwrap();
    for _ in 0..4 {
        let again = run(
            &f.store,
            query("tie probe", None, 10),
            SearchMode::Hybrid,
            &capability,
        )
        .unwrap();
        assert_eq!(again, baseline);
    }
}

#[test]
fn space_and_lifecycle_filters_apply_to_every_mode() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let linux = space(&f.store, "linux");
    let project = space(&f.store, "project");
    f.store.link_spaces(project, coding).unwrap();

    let key = fake_key();
    let a = write(&f.store, "alpha", "retrieval alpha", &[coding], None);
    let b = write(&f.store, "beta", "retrieval beta", &[linux], None);
    let c = write(&f.store, "gamma", "retrieval gamma", &[coding], None);
    let d = write(&f.store, "delta", "retrieval delta first", &[coding], None);
    let e = write(
        &f.store,
        "delta",
        "retrieval delta second",
        &[coding],
        Some(d.memory_id),
    );
    let zeta = write(&f.store, "zeta", "retrieval zeta", &[project], None);
    f.store.retire_memory(c.memory_id, None, Ts::now()).unwrap();

    // The vectors rank the retired and superseded memories highly, so only the
    // filters can keep them out.
    index(&f.store, &key, a.assertion_id, &[1.0, 0.0, 0.0, 0.0]);
    index(&f.store, &key, b.assertion_id, &[0.9, 0.1, 0.0, 0.0]);
    index(&f.store, &key, c.assertion_id, &[0.8, 0.2, 0.0, 0.0]);
    index(&f.store, &key, d.assertion_id, &[0.7, 0.3, 0.0, 0.0]);
    index(&f.store, &key, e.assertion_id, &[0.6, 0.4, 0.0, 0.0]);
    index(&f.store, &key, zeta.assertion_id, &[0.95, 0.05, 0.0, 0.0]);

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert_ready(&capability);

    let cases: Vec<FilterCase> = vec![
        (
            "own space",
            Some(vec![coding]),
            false,
            false,
            vec![a.memory_id, e.memory_id],
        ),
        (
            "other space",
            Some(vec![linux]),
            false,
            false,
            vec![b.memory_id],
        ),
        (
            "traversal via link",
            Some(vec![project]),
            false,
            false,
            vec![zeta.memory_id, a.memory_id, e.memory_id],
        ),
        (
            "superseded included",
            Some(vec![coding]),
            true,
            false,
            vec![a.memory_id, d.memory_id, e.memory_id],
        ),
        (
            "retired included",
            Some(vec![coding]),
            false,
            true,
            vec![a.memory_id, c.memory_id, e.memory_id],
        ),
        (
            "both included",
            Some(vec![coding]),
            true,
            true,
            vec![a.memory_id, c.memory_id, d.memory_id, e.memory_id],
        ),
        (
            "all spaces",
            None,
            false,
            false,
            vec![a.memory_id, b.memory_id, e.memory_id, zeta.memory_id],
        ),
    ];
    for (label, spaces, include_superseded, include_retired, expected) in cases {
        let expected: BTreeSet<Uuid> = expected.into_iter().collect();
        let scoped = |spaces: &Option<Vec<Uuid>>| {
            let mut q = query("retrieval", spaces.clone(), 20);
            q.include_superseded = include_superseded;
            q.include_retired = include_retired;
            q
        };
        let lexical = run(&f.store, scoped(&spaces), SearchMode::Lexical, &capability).unwrap();
        assert_eq!(id_set(&lexical), expected, "{label}: lexical");
        for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
            let outcome = run(&f.store, scoped(&spaces), mode, &capability).unwrap();
            assert_eq!(id_set(&outcome), expected, "{label}: {mode:?}");
            // Decoration must agree with the lexical view of the same memory.
            for hit in &outcome.hits {
                let twin = lexical
                    .hits
                    .iter()
                    .find(|twin| twin.hit.memory_id == hit.hit.memory_id)
                    .unwrap();
                assert_eq!(hit.hit.spaces, twin.hit.spaces, "{label}: {mode:?} spaces");
            }
        }
    }
}

#[test]
fn temporal_filters_apply_to_every_mode() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let key = fake_key();
    let a = write(&f.store, "alpha", "retrieval alpha", &[coding], None);
    index(&f.store, &key, a.assertion_id, &[1.0, 0.0, 0.0, 0.0]);
    let created = retrieval::search(&f.store, query("alpha", None, 5)).unwrap()[0]
        .created_at
        .0;

    // Retire the memory one minute after creation. Retirement accepts a future
    // timestamp, which keeps the timeline deterministic.
    f.store
        .retire_memory(a.memory_id, None, Ts(created + 60_000_000))
        .unwrap();
    let b = write(&f.store, "beta", "retrieval beta", &[coding], None);
    index(&f.store, &key, b.assertion_id, &[0.9, 0.1, 0.0, 0.0]);

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);

    let timeline: [(i64, Vec<Uuid>); 3] = [
        (created - 1, vec![]),
        (created + 30_000_000, vec![a.memory_id, b.memory_id]),
        (created + 120_000_000, vec![b.memory_id]),
    ];
    for (at, expected) in timeline {
        let expected: BTreeSet<Uuid> = expected.into_iter().collect();
        for mode in [
            SearchMode::Lexical,
            SearchMode::Semantic,
            SearchMode::Hybrid,
        ] {
            let mut q = query("retrieval", None, 10);
            q.at = Some(Ts(at));
            let outcome = run(&f.store, q, mode, &capability).unwrap();
            assert_eq!(id_set(&outcome), expected, "at {at} under {mode:?}");
        }
    }
}

#[test]
fn disabled_embeddings_fall_back_in_auto_and_fail_when_explicit() {
    let f = Fixture::new();
    let coding = space(&f.store, "coding");
    write(&f.store, "alpha", "retrieval alpha", &[coding], None);

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert!(
        matches!(
            &capability,
            SemanticCapability::Unavailable(FallbackReason::EmbeddingsDisabled)
        ),
        "{capability:?}"
    );

    let legacy = retrieval::search(&f.store, query("retrieval", None, 10)).unwrap();
    let auto = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Auto,
        &capability,
    )
    .unwrap();
    assert_eq!(auto.requested, SearchMode::Auto);
    assert_eq!(auto.effective, EffectiveMode::Lexical);
    assert_eq!(auto.fallback, Some(FallbackReason::EmbeddingsDisabled));
    let retrieved: Vec<_> = auto.hits.iter().map(|hit| hit.hit.clone()).collect();
    assert_eq!(retrieved, legacy);

    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        let err = run(&f.store, query("retrieval", None, 10), mode, &capability).unwrap_err();
        match &err {
            Error::SemanticUnavailable { reason } => {
                assert!(reason.contains("mind embeddings enable"), "{reason}");
            }
            other => panic!("expected SemanticUnavailable, got {other:?}"),
        }
    }
    assert_eq!(
        calls.factory.load(Ordering::SeqCst),
        0,
        "disabled embeddings must not build a runtime"
    );
    assert_eq!(calls.queries.load(Ordering::SeqCst), 0);
}

#[test]
fn missing_or_malformed_model_files_are_reported_without_building_a_runtime() {
    let f = Fixture::new();
    manager::set_enabled(&f.store, true).unwrap();
    f.store.set_meta(MODEL_META_KEY, FAKE_ID).unwrap();
    let calls = Arc::new(Calls::default());
    let missing = FallbackReason::ModelFilesMissing {
        model: FAKE_ID.to_string(),
    };

    // Nothing is installed. Resolution must not create the directory either.
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert_eq!(reason_of(&capability), Some(&missing));
    assert!(
        !f.base.join("models").exists(),
        "resolution must not create directories"
    );

    // Only the model file is present.
    let dir = manager::model_dir_in(&f.base, FAKE_ID);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("model.safetensors"), MODEL_BYTES).unwrap();
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert_eq!(reason_of(&capability), Some(&missing));

    // The tokenizer has the wrong size.
    fs::write(dir.join("tokenizer.json"), b"short").unwrap();
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert!(
        matches!(
            reason_of(&capability),
            Some(FallbackReason::ModelFilesInvalid { detail, .. })
                if detail.contains("size mismatch")
        ),
        "{capability:?}"
    );

    assert_eq!(
        calls.factory.load(Ordering::SeqCst),
        0,
        "no runtime for invalid files"
    );
}

#[test]
fn readiness_metadata_read_failures_propagate_instead_of_becoming_fallbacks() {
    let f = Fixture::new();
    rusqlite::Connection::open(&f.db)
        .unwrap()
        .execute_batch("DROP TABLE mind_meta")
        .unwrap();
    let calls = Arc::new(Calls::default());
    let factory =
        |_: &ModelManifestEntry, _: &Path| -> mindspace::Result<Box<dyn EmbeddingRuntime>> {
            calls.factory.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeRuntime {
                answer: Some(vec![1.0, 0.0, 0.0, 0.0]),
                calls: Arc::clone(&calls),
            }))
        };

    let error = resolve_with(&f.store, &f.base, lookup, &factory).unwrap_err();
    assert!(matches!(error, Error::Storage(_)), "{error:?}");
    assert_eq!(calls.factory.load(Ordering::SeqCst), 0);
}

#[cfg(unix)]
#[test]
fn symlinked_model_artifacts_are_refused() {
    let f = Fixture::new();
    manager::set_enabled(&f.store, true).unwrap();
    f.store.set_meta(MODEL_META_KEY, FAKE_ID).unwrap();
    write_artifacts(&f.base, MODEL_BYTES, TOKENIZER_BYTES);
    let dir = manager::model_dir_in(&f.base, FAKE_ID);
    fs::remove_file(dir.join("tokenizer.json")).unwrap();
    std::os::unix::fs::symlink(dir.join("model.safetensors"), dir.join("tokenizer.json")).unwrap();

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert!(
        matches!(
            reason_of(&capability),
            Some(FallbackReason::ModelFilesInvalid { detail, .. })
                if detail.contains("symlinked")
        ),
        "{capability:?}"
    );
    assert_eq!(calls.factory.load(Ordering::SeqCst), 0);
}

#[cfg(unix)]
#[test]
fn symlinked_data_directory_is_refused() {
    let f = Fixture::new();
    manager::set_enabled(&f.store, true).unwrap();
    f.store.set_meta(MODEL_META_KEY, FAKE_ID).unwrap();

    let target = f._dir.path().join("model-data");
    write_artifacts(&target, MODEL_BYTES, TOKENIZER_BYTES);
    std::os::unix::fs::symlink(&target, &f.base).unwrap();

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert!(
        matches!(
            reason_of(&capability),
            Some(FallbackReason::ModelFilesInvalid { detail, .. })
                if detail.contains("symlinked")
        ),
        "{capability:?}"
    );
    assert_eq!(calls.factory.load(Ordering::SeqCst), 0);
}

#[test]
fn checksum_mismatch_is_rejected_before_the_runtime_is_built() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let a = write(&f.store, "alpha", "retrieval alpha", &[coding], None);
    // Indexing comes first so that the coverage check passes and hashing runs.
    index(&f.store, &fake_key(), a.assertion_id, &[1.0, 0.0, 0.0, 0.0]);

    // Same size, different bytes.
    let mut tampered = MODEL_BYTES.to_vec();
    tampered[0] ^= 0x01;
    write_artifacts(&f.base, &tampered, TOKENIZER_BYTES);

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert!(
        matches!(
            reason_of(&capability),
            Some(FallbackReason::ModelFilesInvalid { detail, .. })
                if detail.contains("checksum mismatch")
        ),
        "{capability:?}"
    );
    assert_eq!(
        calls.factory.load(Ordering::SeqCst),
        0,
        "verification precedes runtime construction"
    );
}

#[test]
fn missing_compatible_index_is_reported() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let a = write(&f.store, "alpha", "retrieval alpha", &[coding], None);
    let calls = Arc::new(Calls::default());

    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert_eq!(
        reason_of(&capability),
        Some(&FallbackReason::NoCompatibleIndex)
    );

    // Vectors exist, but only under a different derivation key.
    index(
        &f.store,
        &alternate_key(),
        a.assertion_id,
        &[1.0, 0.0, 0.0, 0.0],
    );
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert_eq!(
        reason_of(&capability),
        Some(&FallbackReason::NoCompatibleIndex)
    );
    assert_eq!(calls.factory.load(Ordering::SeqCst), 0);

    let auto = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Auto,
        &capability,
    )
    .unwrap();
    assert_eq!(auto.effective, EffectiveMode::Lexical);
    assert_eq!(auto.fallback, Some(FallbackReason::NoCompatibleIndex));
    assert_eq!(ids(&auto), vec![a.memory_id]);

    let err = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Semantic,
        &capability,
    )
    .unwrap_err();
    assert!(err.to_string().contains("mind index run"), "{err}");
}

#[test]
fn unknown_model_falls_back_in_auto_and_errors_in_explicit_modes() {
    let f = Fixture::new();
    manager::set_enabled(&f.store, true).unwrap();
    let unknown_id = "not-in-the-manifest";
    f.store.set_meta(MODEL_META_KEY, unknown_id).unwrap();
    let coding = space(&f.store, "coding");
    let note = write(&f.store, "alpha", "retrieval alpha", &[coding], None);

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    let expected = FallbackReason::UnknownModel {
        id: unknown_id.to_string(),
    };
    assert_eq!(reason_of(&capability), Some(&expected));

    let auto = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Auto,
        &capability,
    )
    .unwrap();
    assert_eq!(auto.requested, SearchMode::Auto);
    assert_eq!(auto.effective, EffectiveMode::Lexical);
    assert_eq!(auto.fallback, Some(expected.clone()));
    assert_eq!(ids(&auto), vec![note.memory_id]);

    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        let error = run(&f.store, query("retrieval", None, 10), mode, &capability).unwrap_err();
        assert!(
            matches!(
                &error,
                Error::SemanticUnavailable { reason }
                    if reason.contains(unknown_id) && reason.contains("mind model list")
            ),
            "{mode:?}: {error}"
        );
    }
    assert_eq!(calls.factory.load(Ordering::SeqCst), 0);
    assert_eq!(calls.queries.load(Ordering::SeqCst), 0);
}

#[test]
fn runtime_factory_failure_falls_back_in_auto_and_errors_in_explicit_modes() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let note = write(&f.store, "alpha", "retrieval alpha", &[coding], None);
    index(
        &f.store,
        &fake_key(),
        note.assertion_id,
        &[1.0, 0.0, 0.0, 0.0],
    );

    let calls = Arc::new(Calls::default());
    let factory_calls = Arc::clone(&calls);
    let factory =
        move |_: &ModelManifestEntry, _: &Path| -> mindspace::Result<Box<dyn EmbeddingRuntime>> {
            factory_calls.factory.fetch_add(1, Ordering::SeqCst);
            Err(Error::Storage("synthetic runtime init failure".to_string()))
        };
    let capability = resolve_with(&f.store, &f.base, lookup, &factory).unwrap();
    let expected = FallbackReason::RuntimeUnavailable {
        detail: "synthetic runtime init failure".to_string(),
    };
    assert_eq!(reason_of(&capability), Some(&expected));

    let auto = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Auto,
        &capability,
    )
    .unwrap();
    assert_eq!(auto.requested, SearchMode::Auto);
    assert_eq!(auto.effective, EffectiveMode::Lexical);
    assert_eq!(auto.fallback, Some(expected.clone()));
    assert_eq!(ids(&auto), vec![note.memory_id]);

    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        let error = run(&f.store, query("retrieval", None, 10), mode, &capability).unwrap_err();
        assert!(
            matches!(
                &error,
                Error::SemanticUnavailable { reason }
                    if reason.contains("embedding runtime unavailable")
                        && reason.contains("synthetic runtime init failure")
            ),
            "{mode:?}: {error}"
        );
    }
    assert_eq!(calls.factory.load(Ordering::SeqCst), 1);
    assert_eq!(calls.queries.load(Ordering::SeqCst), 0);
}

#[test]
fn auto_falls_back_to_lexical_with_a_reason_and_hybrid_when_ready() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let linux = space(&f.store, "linux");
    let a = write(&f.store, "alpha", "retrieval alpha", &[coding], None);
    let b = write(&f.store, "beta", "retrieval beta", &[linux], None);
    // b is deliberately left unindexed, so the linux space has no vectors.
    index(&f.store, &fake_key(), a.assertion_id, &[1.0, 0.0, 0.0, 0.0]);
    let calls = Arc::new(Calls::default());
    let healthy = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    assert_ready(&healthy);

    // No semantic candidate survives the space filter.
    let linux_only = run(
        &f.store,
        query("retrieval", Some(vec![linux]), 10),
        SearchMode::Auto,
        &healthy,
    )
    .unwrap();
    assert_eq!(linux_only.effective, EffectiveMode::Lexical);
    assert_eq!(
        linux_only.fallback,
        Some(FallbackReason::NoSemanticCandidates)
    );
    assert_eq!(ids(&linux_only), vec![b.memory_id]);
    assert_eq!(
        (
            linux_only.lexical_candidates,
            linux_only.semantic_candidates
        ),
        (1, 0)
    );

    // When the eligible lexical pool contains multiple episodes of one
    // assertion, a semantic-empty fallback remains episode-level.
    let history = space(&f.store, "history");
    let old = write(
        &f.store,
        "gamma",
        "episode fallback probe",
        &[history],
        None,
    );
    f.store
        .retire_memory(old.memory_id, None, Ts::now())
        .unwrap();
    let current = write(
        &f.store,
        "gamma",
        "episode fallback probe",
        &[history],
        None,
    );
    assert_eq!(old.assertion_id, current.assertion_id);
    let mut history_query = query("episode fallback", Some(vec![history]), 10);
    history_query.include_retired = true;
    let lexical_history = retrieval::search(&f.store, history_query.clone()).unwrap();
    assert_eq!(lexical_history.len(), 2);
    for mode in [SearchMode::Auto, SearchMode::Hybrid] {
        let fallback = run(&f.store, history_query.clone(), mode, &healthy).unwrap();
        assert_eq!(fallback.effective, EffectiveMode::Lexical);
        assert_eq!(
            fallback.fallback,
            Some(FallbackReason::NoSemanticCandidates)
        );
        assert_eq!(fallback.lexical_candidates, 2);
        assert_eq!(
            fallback
                .hits
                .iter()
                .map(|hit| hit.hit.clone())
                .collect::<Vec<_>>(),
            lexical_history
        );
    }

    let mut limited_history = query("episode fallback", Some(vec![history]), 1);
    limited_history.include_retired = true;
    let lexical_limit = run(
        &f.store,
        limited_history.clone(),
        SearchMode::Lexical,
        &healthy,
    )
    .unwrap();
    let auto_limit = run(&f.store, limited_history, SearchMode::Auto, &healthy).unwrap();
    assert_eq!(lexical_limit.lexical_candidates, 1);
    assert_eq!(auto_limit.lexical_candidates, 2);
    assert_eq!(ids(&auto_limit), ids(&lexical_limit));

    // The query embedding fails at runtime.
    let failing = f.resolve(None, &calls);
    let outcome = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Auto,
        &failing,
    )
    .unwrap();
    assert_eq!(outcome.effective, EffectiveMode::Lexical);
    assert!(
        matches!(
            &outcome.fallback,
            Some(FallbackReason::QueryEmbeddingFailed { detail })
                if detail.contains("fake query failure")
        ),
        "{:?}",
        outcome.fallback
    );
    let legacy = retrieval::search(&f.store, query("retrieval", None, 10)).unwrap();
    let retrieved: Vec<_> = outcome.hits.iter().map(|hit| hit.hit.clone()).collect();
    assert_eq!(retrieved, legacy);

    // A wrong dimension, then a non-finite value.
    for (answer, expected) in [
        (vec![1.0, 0.0, 0.0], "dimension mismatch"),
        (vec![f32::NAN, 0.0, 0.0, 0.0], "non-finite"),
    ] {
        let capability = f.resolve(Some(answer), &calls);
        let outcome = run(
            &f.store,
            query("retrieval", None, 10),
            SearchMode::Auto,
            &capability,
        )
        .unwrap();
        assert_eq!(outcome.effective, EffectiveMode::Lexical);
        assert!(
            matches!(
                &outcome.fallback,
                Some(FallbackReason::QueryEmbeddingFailed { detail })
                    if detail.contains(expected)
            ),
            "{:?}",
            outcome.fallback
        );
    }

    // With everything healthy, Auto runs hybrid.
    let outcome = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Auto,
        &healthy,
    )
    .unwrap();
    assert_eq!(outcome.effective, EffectiveMode::Hybrid);
    assert_eq!(outcome.fallback, None);
    assert!(outcome.semantic_candidates >= 1);
}

#[test]
fn auto_and_hybrid_report_candidate_fallback_when_both_branches_are_empty() {
    let f = Fixture::new();
    f.enable_with_files();
    let indexed_space = space(&f.store, "indexed");
    let filtered_space = space(&f.store, "filtered");
    let indexed = write(
        &f.store,
        "indexed note",
        "content outside the requested space",
        &[indexed_space],
        None,
    );
    index(
        &f.store,
        &fake_key(),
        indexed.assertion_id,
        &[1.0, 0.0, 0.0, 0.0],
    );
    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);

    let scoped = query("no lexical match", Some(vec![filtered_space]), 5);
    for mode in [SearchMode::Hybrid, SearchMode::Auto] {
        let outcome = run(&f.store, scoped.clone(), mode, &capability).unwrap();
        assert_eq!(outcome.requested, mode);
        assert_eq!(outcome.effective, EffectiveMode::Lexical);
        assert_eq!(outcome.fallback, Some(FallbackReason::NoSemanticCandidates));
        assert_eq!(outcome.lexical_candidates, 0);
        assert_eq!(outcome.semantic_candidates, 0);
        assert!(outcome.hits.is_empty());
        let wire = serde_json::to_value(&outcome).unwrap();
        assert_eq!(wire["fallback"]["kind"], "no_semantic_candidates");
        assert_eq!(wire["hits"], serde_json::json!([]));
    }
}

#[test]
fn explicit_semantic_modes_fail_with_actionable_errors() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let a = write(&f.store, "alpha", "retrieval alpha", &[coding], None);
    index(&f.store, &fake_key(), a.assertion_id, &[1.0, 0.0, 0.0, 0.0]);
    let calls = Arc::new(Calls::default());

    let failing = f.resolve(None, &calls);
    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        let err = run(&f.store, query("retrieval", None, 10), mode, &failing).unwrap_err();
        assert!(
            matches!(
                &err,
                Error::SemanticUnavailable { reason } if reason.contains("query embedding failed")
            ),
            "{mode:?}: {err}"
        );
    }

    let wrong_size = f.resolve(Some(vec![1.0]), &calls);
    let err = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Semantic,
        &wrong_size,
    )
    .unwrap_err();
    assert!(err.to_string().contains("dimension mismatch"), "{err}");

    let err = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Hybrid,
        &unavailable(FallbackReason::NoCompatibleIndex),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "requested retrieval mode unavailable: no vectors are indexed for the selected model; run: mind index run"
    );
}

#[cfg(not(feature = "embeddings"))]
#[test]
fn builds_without_embeddings_report_the_missing_feature() {
    let f = Fixture::new();
    let coding = space(&f.store, "coding");
    write(&f.store, "alpha", "retrieval alpha", &[coding], None);

    let capability = retrieval::semantic::resolve(&f.store).unwrap();
    assert_eq!(
        reason_of(&capability),
        Some(&FallbackReason::FeatureDisabled)
    );

    let auto = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Auto,
        &capability,
    )
    .unwrap();
    assert_eq!(auto.effective, EffectiveMode::Lexical);
    assert_eq!(auto.fallback, Some(FallbackReason::FeatureDisabled));
    assert_eq!(auto.hits.len(), 1);

    let err = run(
        &f.store,
        query("retrieval", None, 10),
        SearchMode::Semantic,
        &capability,
    )
    .unwrap_err();
    assert!(err.to_string().contains("--features embeddings"), "{err}");
}

#[cfg(feature = "embeddings")]
#[test]
fn production_resolution_on_a_fresh_database_is_disabled() {
    let f = Fixture::new();
    let capability = retrieval::semantic::resolve(&f.store).unwrap();
    assert_eq!(
        reason_of(&capability),
        Some(&FallbackReason::EmbeddingsDisabled)
    );
}

#[test]
fn search_has_no_indexing_download_or_write_side_effects() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let a = write(&f.store, "alpha", "retrieval alpha", &[coding], None);
    write(&f.store, "beta", "retrieval beta", &[coding], None);
    index(&f.store, &fake_key(), a.assertion_id, &[1.0, 0.0, 0.0, 0.0]);

    let calls = Arc::new(Calls::default());
    let before_db = snapshot_db(&f.db);
    let before_files = snapshot_tree(&f.base);

    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    for mode in [
        SearchMode::Lexical,
        SearchMode::Semantic,
        SearchMode::Hybrid,
        SearchMode::Auto,
    ] {
        run(&f.store, query("retrieval", None, 10), mode, &capability).unwrap();
    }
    retrieval::search(&f.store, query("retrieval", None, 10)).unwrap();

    assert_eq!(
        calls.documents.load(Ordering::SeqCst),
        0,
        "search must not index"
    );
    assert_eq!(
        calls.queries.load(Ordering::SeqCst),
        3,
        "Semantic, Hybrid, and Auto embed; Lexical must not"
    );
    assert_eq!(
        calls.factory.load(Ordering::SeqCst),
        1,
        "only resolution builds a runtime"
    );
    assert_eq!(
        snapshot_db(&f.db),
        before_db,
        "search must not write rows or metadata"
    );
    assert_eq!(
        snapshot_tree(&f.base),
        before_files,
        "search must not create or change model files"
    );
}

#[test]
fn blank_queries_are_rejected_and_zero_limit_skips_embedding() {
    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let a = write(&f.store, "alpha", "retrieval alpha", &[coding], None);
    index(&f.store, &fake_key(), a.assertion_id, &[1.0, 0.0, 0.0, 0.0]);
    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);

    for mode in [
        SearchMode::Lexical,
        SearchMode::Semantic,
        SearchMode::Hybrid,
        SearchMode::Auto,
    ] {
        let err = run(&f.store, query("   ", None, 10), mode, &capability).unwrap_err();
        assert!(matches!(&err, Error::EmptyQuery), "{mode:?}: {err}");
        let outcome = run(&f.store, query("retrieval", None, 0), mode, &capability).unwrap();
        assert!(outcome.hits.is_empty(), "{mode:?}");
        assert_eq!(outcome.effective, EffectiveMode::NotRun, "{mode:?}");
    }
    assert_eq!(
        calls.queries.load(Ordering::SeqCst),
        0,
        "a zero limit must not embed the query"
    );
}

#[test]
fn options_control_the_fusion_constant_and_candidate_pool() {
    let f = Fixture::new();
    f.enable_with_files();
    seed_disagreeing_rankings(&f);
    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);

    let options = RetrievalOptions {
        rrf_k: NonZeroU32::new(1).unwrap(),
        candidate_pool: Some(1),
    };
    let outcome = retrieval::search_with_mode(
        &f.store,
        query("alpha", None, 1),
        SearchMode::Hybrid,
        &capability,
        options,
    )
    .unwrap();
    assert_eq!(
        outcome.lexical_candidates, 1,
        "the pool override bounds the lexical branch"
    );
    assert_eq!(
        outcome.semantic_candidates, 1,
        "the pool override bounds the semantic branch"
    );
    assert_eq!(outcome.hits.len(), 1);
    // Each branch contributes one rank-1 candidate, and k = 1 gives 1 / (1 + 1).
    assert!(
        (outcome.hits[0].hit.score - 0.5).abs() < 1e-12,
        "{}",
        outcome.hits[0].hit.score
    );
}

#[test]
fn mode_aware_outcomes_serialize_stable_modes_scores_and_provenance() {
    let modes = [
        (SearchMode::Lexical, "lexical"),
        (SearchMode::Semantic, "semantic"),
        (SearchMode::Hybrid, "hybrid"),
        (SearchMode::Auto, "auto"),
    ];
    for (mode, wire) in modes {
        assert_eq!(serde_json::to_value(mode).unwrap(), wire);
    }
    assert_eq!(
        serde_json::to_value(EffectiveMode::NotRun).unwrap(),
        "not_run"
    );
    for (kind, wire) in [
        (ScoreKind::Lexical, "lexical"),
        (ScoreKind::Semantic, "semantic"),
        (ScoreKind::Fused, "fused"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), wire);
    }

    let fallback_cases = [
        (
            FallbackReason::EmbeddingsDisabled,
            serde_json::json!({"kind":"embeddings_disabled"}),
        ),
        (
            FallbackReason::UnknownModel {
                id: "unknown-model".to_string(),
            },
            serde_json::json!({"kind":"unknown_model","id":"unknown-model"}),
        ),
        (
            FallbackReason::ModelFilesInvalid {
                model: "test-model".to_string(),
                detail: "digest mismatch".to_string(),
            },
            serde_json::json!({"kind":"model_files_invalid","model":"test-model","detail":"digest mismatch"}),
        ),
        (
            FallbackReason::NoSemanticCandidates,
            serde_json::json!({"kind":"no_semantic_candidates"}),
        ),
    ];
    for (reason, wire) in fallback_cases {
        assert_eq!(serde_json::to_value(reason).unwrap(), wire);
    }

    let f = Fixture::new();
    f.enable_with_files();
    let coding = space(&f.store, "coding");
    let note = write(
        &f.store,
        "wire contract",
        "retrieval wire contract",
        &[coding],
        None,
    );
    index(
        &f.store,
        &fake_key(),
        note.assertion_id,
        &[1.0, 0.0, 0.0, 0.0],
    );

    let lexical = run(
        &f.store,
        query("retrieval wire", None, 5),
        SearchMode::Lexical,
        &unavailable(FallbackReason::EmbeddingsDisabled),
    )
    .unwrap();
    let lexical_wire = serde_json::to_value(&lexical).unwrap();
    assert_eq!(lexical_wire["requested"], "lexical");
    assert_eq!(lexical_wire["effective"], "lexical");
    assert_eq!(lexical_wire["fallback"], serde_json::Value::Null);
    assert_eq!(lexical_wire["hits"][0]["score_kind"], "lexical");
    assert_eq!(lexical_wire["hits"][0]["provenance"]["lexical_rank"], 1);
    assert_eq!(
        lexical_wire["hits"][0]["provenance"]["semantic_score"],
        serde_json::Value::Null
    );
    assert_eq!(
        lexical_wire["hits"][0]["hit"]["score"],
        lexical_wire["hits"][0]["provenance"]["lexical_score"]
    );

    let calls = Arc::new(Calls::default());
    let capability = f.resolve(Some(vec![1.0, 0.0, 0.0, 0.0]), &calls);
    let semantic = run(
        &f.store,
        query("retrieval wire", None, 5),
        SearchMode::Semantic,
        &capability,
    )
    .unwrap();
    let semantic_wire = serde_json::to_value(&semantic).unwrap();
    assert_eq!(semantic_wire["effective"], "semantic");
    assert_eq!(semantic_wire["hits"][0]["score_kind"], "semantic");
    assert_eq!(
        semantic_wire["hits"][0]["provenance"]["lexical_rank"],
        serde_json::Value::Null
    );
    assert_eq!(semantic_wire["hits"][0]["provenance"]["semantic_rank"], 1);

    let hybrid = run(
        &f.store,
        query("retrieval wire", None, 5),
        SearchMode::Hybrid,
        &capability,
    )
    .unwrap();
    let hybrid_wire = serde_json::to_value(&hybrid).unwrap();
    assert_eq!(hybrid_wire["effective"], "hybrid");
    assert_eq!(hybrid_wire["hits"][0]["score_kind"], "fused");
    assert_eq!(hybrid_wire["hits"][0]["provenance"]["lexical_rank"], 1);
    assert_eq!(hybrid_wire["hits"][0]["provenance"]["semantic_rank"], 1);
    assert!(hybrid_wire["hits"][0]["provenance"]["lexical_score"].is_number());
    assert!(hybrid_wire["hits"][0]["provenance"]["semantic_score"].is_number());
}

#[test]
fn resolved_mode_adapter_searches_without_mutating_storage() {
    let f = Fixture::new();
    let coding = space(&f.store, "coding");
    write(
        &f.store,
        "adapter",
        "resolved adapter marker",
        &[coding],
        None,
    );
    let before = snapshot_db(&f.db);

    let lexical = retrieval::search_with_mode_resolving_capability(
        &f.store,
        query("resolved adapter", None, 5),
        SearchMode::Lexical,
    )
    .unwrap();
    assert_eq!(lexical.effective, EffectiveMode::Lexical);
    assert_eq!(lexical.hits.len(), 1);

    let auto = retrieval::search_with_mode_resolving_capability(
        &f.store,
        query("resolved adapter", None, 5),
        SearchMode::Auto,
    )
    .unwrap();
    assert_eq!(auto.effective, EffectiveMode::Lexical);
    assert_eq!(auto.hits.len(), 1);
    assert!(auto.fallback.is_some());
    assert_eq!(snapshot_db(&f.db), before);
}
