use super::*;
use crate::core::{AssertionEmbedding, ManualMemoryDraft};
use crate::embeddings::manifest::{Artifact, ModelManifestEntry, NOMIC_V1_5};
use crate::embeddings::EmbeddingRuntime;
use crate::retrieval::EffectiveMode;
use crate::SqliteStore;
use std::cell::Cell;
use std::fs::FileTimes;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

static MODEL: ModelManifestEntry = ModelManifestEntry {
    id: "cache-test",
    dimension: 2,
    artifacts: &[
        Artifact {
            filename: "model.safetensors",
            url: "https://example.invalid/model",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            size: 3,
        },
        Artifact {
            filename: "tokenizer.json",
            url: "https://example.invalid/tokenizer",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            size: 3,
        },
    ],
    ..NOMIC_V1_5
};

static OTHER: ModelManifestEntry = ModelManifestEntry {
    id: "cache-other",
    ..MODEL
};

static CHANGED_MANIFEST: ModelManifestEntry = ModelManifestEntry {
    prefix_query: "different query prefix: ",
    ..MODEL
};

fn changed_lookup(id: &str) -> Option<&'static ModelManifestEntry> {
    (id == MODEL.id).then_some(&CHANGED_MANIFEST)
}

fn lookup(id: &str) -> Option<&'static ModelManifestEntry> {
    match id {
        "cache-test" => Some(&MODEL),
        "cache-other" => Some(&OTHER),
        _ => None,
    }
}

#[derive(Default)]
struct Calls {
    factories: AtomicUsize,
    queries: AtomicUsize,
    fail: AtomicBool,
    on_factory: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    on_query: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

struct Runtime {
    id: String,
    calls: Arc<Calls>,
}

impl EmbeddingRuntime for Runtime {
    fn embed_document(&self, _: &str) -> Result<Vec<f32>> {
        panic!("retrieval must not index")
    }
    fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
        self.calls.queries.fetch_add(1, Ordering::SeqCst);
        if let Some(action) = self.calls.on_query.lock().unwrap().take() {
            action();
        }
        if self.calls.fail.load(Ordering::SeqCst) {
            return Err(Error::Storage("query failed".into()));
        }
        Ok(vec![1.0, 0.0])
    }
    fn model_id(&self) -> &str {
        &self.id
    }
    fn dimension(&self) -> usize {
        2
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    store: SqliteStore,
    calls: Arc<Calls>,
    now: Cell<SystemTime>,
    lookup: Cell<ModelLookup>,
}

impl Fixture {
    fn new() -> Self {
        let f = Self {
            dir: tempfile::tempdir().unwrap(),
            store: SqliteStore::open_in_memory().unwrap(),
            calls: Arc::default(),
            now: Cell::new(SystemTime::now() + Duration::from_secs(10)),
            lookup: Cell::new(lookup),
        };
        let space = f.store.create_space("test", "").unwrap().id;
        let note = f
            .store
            .record_manual_write(ManualMemoryDraft {
                subject: Some("marker".into()),
                content: "retrieval marker".into(),
                spaces: vec![space],
                supersedes: None,
            })
            .unwrap();
        for entry in [&MODEL, &OTHER] {
            let dir = super::super::manager::model_dir_in(f.dir.path(), entry.id);
            fs::create_dir_all(&dir).unwrap();
            for art in entry.artifacts {
                fs::write(dir.join(art.filename), b"abc").unwrap();
            }
            f.store
                .save_embeddings(&[AssertionEmbedding {
                    assertion_id: note.assertion_id,
                    derivation_key: super::super::manifest::derivation_from_manifest(entry)
                        .derivation_key(),
                    model: entry.id.into(),
                    model_version: entry.version.into(),
                    dimension: 2,
                    embedding: vec![1.0, 0.0],
                }])
                .unwrap();
        }
        f.store.set_meta("embedding.enabled", "true").unwrap();
        f.store.set_meta("embedding.model", MODEL.id).unwrap();
        f
    }

    fn artifact(&self) -> std::path::PathBuf {
        super::super::manager::model_dir_in(self.dir.path(), MODEL.id).join("model.safetensors")
    }

    fn run(&self, cache: &mut SemanticCache, mode: SearchMode) -> Result<SearchOutcome> {
        self.run_policy(cache, mode, 5, true)
    }

    fn run_policy(
        &self,
        cache: &mut SemanticCache,
        mode: SearchMode,
        limit: u32,
        qualify: bool,
    ) -> Result<SearchOutcome> {
        cache.search_with(
            &self.store,
            SearchQuery {
                text: "retrieval marker".into(),
                space_ids: None,
                limit,
                at: None,
                include_superseded: false,
                include_retired: false,
            },
            mode,
            self.dir.path(),
            self.lookup.get(),
            &|entry, _| {
                self.calls.factories.fetch_add(1, Ordering::SeqCst);
                if let Some(action) = self.calls.on_factory.lock().unwrap().take() {
                    action();
                }
                Ok(Box::new(Runtime {
                    id: entry.id.into(),
                    calls: self.calls.clone(),
                }))
            },
            &|| self.now.get(),
            &|_| qualify,
        )
    }

    fn factories(&self) -> usize {
        self.calls.factories.load(Ordering::SeqCst)
    }
}

#[test]
fn cold_verifies_and_warm_modes_share_one_runtime_but_sessions_do_not() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    for mode in [SearchMode::Semantic, SearchMode::Hybrid, SearchMode::Auto] {
        let out = f.run(&mut cache, mode).unwrap();
        assert!(!out.hits.is_empty());
        assert!(cache.entry.is_some());
    }
    assert_eq!(f.factories(), 1);
    assert_eq!(f.calls.queries.load(Ordering::SeqCst), 3);
    f.run(&mut SemanticCache::default(), SearchMode::Semantic)
        .unwrap();
    assert_eq!(f.factories(), 2);
}

#[test]
fn cold_corruption_never_constructs_runtime_and_warm_same_size_corruption_invalidates() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    let path = f.artifact();
    let original = fs::metadata(&path).unwrap();
    fs::write(&path, b"bad").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(original.modified().unwrap()))
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), original.len());
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        original.modified().unwrap()
    );
    let out = f.run(&mut cache, SearchMode::Auto).unwrap();
    assert!(matches!(
        out.fallback,
        Some(FallbackReason::ModelFilesInvalid { .. })
    ));
    assert!(cache.entry.is_none());
    assert_eq!(f.factories(), 1);
    assert!(f
        .run(&mut SemanticCache::default(), SearchMode::Semantic)
        .is_err());
    assert_eq!(f.factories(), 1);
}

#[test]
fn enablement_selection_and_missing_files_evict() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    f.store.set_meta("embedding.enabled", "false").unwrap();
    let disabled = f.run(&mut cache, SearchMode::Auto).unwrap();
    assert_eq!(disabled.fallback, Some(FallbackReason::EmbeddingsDisabled));
    assert!(cache.entry.is_none());
    f.store.set_meta("embedding.enabled", "true").unwrap();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    f.store.set_meta("embedding.model", OTHER.id).unwrap();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert_eq!(cache.entry.as_ref().unwrap().model.entry.id, OTHER.id);
    f.store.set_meta("embedding.model", MODEL.id).unwrap();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert_eq!(f.factories(), 4);
    fs::remove_file(f.artifact()).unwrap();
    assert!(f.run(&mut cache, SearchMode::Semantic).is_err());
    assert!(cache.entry.is_none());
}

#[test]
fn query_errors_evict_even_when_auto_returns_success() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    f.calls.fail.store(true, Ordering::SeqCst);
    let out = f.run(&mut cache, SearchMode::Auto).unwrap();
    assert!(matches!(
        out.fallback,
        Some(FallbackReason::QueryEmbeddingFailed { .. })
    ));
    assert!(cache.entry.is_none());
    f.calls.fail.store(false, Ordering::SeqCst);
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert_eq!(f.factories(), 2);
}

#[test]
fn mutation_during_construction_or_query_discards_results_without_retry() {
    for during_query in [false, true] {
        let f = Fixture::new();
        let mut cache = SemanticCache::default();
        let path = f.artifact();
        let action: Box<dyn FnOnce() + Send> = Box::new(move || fs::write(path, b"bad").unwrap());
        if during_query {
            *f.calls.on_query.lock().unwrap() = Some(action);
        } else {
            *f.calls.on_factory.lock().unwrap() = Some(action);
        }
        let out = f.run(&mut cache, SearchMode::Auto).unwrap();
        assert_eq!(out.effective, EffectiveMode::Lexical);
        assert!(matches!(
            out.fallback,
            Some(FallbackReason::ModelFilesInvalid { .. })
        ));
        assert_eq!(f.factories(), 1);
        assert_eq!(f.calls.queries.load(Ordering::SeqCst), 1);
        assert!(cache.entry.is_none());
    }
}

#[test]
fn zero_limit_does_not_load_or_publish_but_warm_entry_can_survive() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    let zero = f.run_policy(&mut cache, SearchMode::Auto, 0, true).unwrap();
    assert_eq!(zero.effective, EffectiveMode::NotRun);
    assert_eq!(f.calls.queries.load(Ordering::SeqCst), 0);
    assert!(cache.entry.is_none());
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    f.run_policy(&mut cache, SearchMode::Auto, 0, true).unwrap();
    assert!(cache.entry.is_some());
    assert_eq!(f.factories(), 2);
}

#[test]
fn unqualified_filesystem_recent_artifacts_and_clock_rollback_never_reuse() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    for _ in 0..2 {
        f.run_policy(&mut cache, SearchMode::Semantic, 5, false)
            .unwrap();
        assert!(cache.entry.is_none());
    }
    f.now.set(SystemTime::now());
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert!(cache.entry.is_none());
    f.now.set(SystemTime::now() + Duration::from_secs(10));
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert!(cache.entry.is_some());
    f.now.set(f.now.get() - Duration::from_secs(1));
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert!(cache.entry.is_none());
    assert_eq!(f.factories(), 5);
}

#[test]
fn lexical_never_resolves_even_with_broken_artifacts() {
    let f = Fixture::new();
    fs::remove_file(f.artifact()).unwrap();
    let mut cache = SemanticCache::default();
    let out = f.run(&mut cache, SearchMode::Lexical).unwrap();
    assert_eq!(out.effective, EffectiveMode::Lexical);
    assert_eq!(f.factories(), 0);
    assert_eq!(f.calls.queries.load(Ordering::SeqCst), 0);
}

#[test]
fn identity_requires_two_seconds_and_rejects_invalid_or_future_ctime() {
    let mut id = Identity::read(Fixture::new().artifact().as_path()).unwrap();
    id.ctime = (100, 0);
    assert!(!id.quiescent(UNIX_EPOCH + Duration::from_millis(101_999)));
    assert!(id.quiescent(UNIX_EPOCH + Duration::from_secs(102)));
    assert!(!id.quiescent(UNIX_EPOCH + Duration::from_secs(99)));
    id.ctime = (-1, 0);
    assert!(!id.quiescent(SystemTime::now()));
    id.ctime = (100, 1_000_000_000);
    assert!(!id.quiescent(SystemTime::now()));
}

#[test]
fn index_removal_and_restoration_are_seen_without_caching_vectors() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    let first = f.run(&mut cache, SearchMode::Semantic).unwrap();
    let assertion_id = f
        .store
        .get_memory_detail(first.hits[0].hit.memory_id)
        .unwrap()
        .unwrap()
        .assertion
        .id;
    let key = super::super::manifest::derivation_from_manifest(&MODEL).derivation_key();
    f.store.clear_embeddings(&key).unwrap();
    let out = f.run(&mut cache, SearchMode::Auto).unwrap();
    assert_eq!(out.fallback, Some(FallbackReason::NoCompatibleIndex));
    assert!(cache.entry.is_none());
    f.store
        .save_embeddings(&[AssertionEmbedding {
            assertion_id,
            derivation_key: key,
            model: MODEL.id.into(),
            model_version: MODEL.version.into(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert_eq!(f.factories(), 2);
    f.store.set_meta("embedding.model", OTHER.id).unwrap();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert_eq!(f.factories(), 3);
}

#[test]
fn replacement_and_symlink_substitution_invalidate_warm_sources() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    let path = f.artifact();
    let replacement = path.with_extension("replacement");
    fs::write(&replacement, b"abc").unwrap();
    fs::rename(&replacement, &path).unwrap();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert_eq!(f.factories(), 2);
    fs::rename(&path, &replacement).unwrap();
    std::os::unix::fs::symlink(&replacement, &path).unwrap();
    let out = f.run(&mut cache, SearchMode::Auto).unwrap();
    assert!(matches!(
        out.fallback,
        Some(FallbackReason::ModelFilesInvalid { .. })
    ));
    assert!(cache.entry.is_none());
}

#[test]
fn original_query_failure_is_not_masked_by_post_query_artifact_failure() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    let path = f.artifact();
    *f.calls.on_query.lock().unwrap() = Some(Box::new(move || fs::remove_file(path).unwrap()));
    f.calls.fail.store(true, Ordering::SeqCst);
    let error = f.run(&mut cache, SearchMode::Semantic).unwrap_err();
    assert!(error.to_string().contains("query failed"));
    assert!(!error.to_string().contains("changed during retrieval"));
    assert!(cache.entry.is_none());
}

#[test]
fn full_manifest_not_just_derivation_key_identifies_a_runtime() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert_eq!(
        super::super::manifest::derivation_from_manifest(&MODEL).derivation_key(),
        super::super::manifest::derivation_from_manifest(&CHANGED_MANIFEST).derivation_key()
    );
    f.lookup.set(changed_lookup);
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert_eq!(f.factories(), 2);
    assert_eq!(
        cache.entry.as_ref().unwrap().model.entry.prefix_query,
        CHANGED_MANIFEST.prefix_query
    );
}

#[test]
fn warm_post_query_mutation_fails_strict_modes_and_auto_falls_back() {
    for mode in [SearchMode::Semantic, SearchMode::Hybrid, SearchMode::Auto] {
        let f = Fixture::new();
        let mut cache = SemanticCache::default();
        f.run(&mut cache, SearchMode::Semantic).unwrap();
        let path = f.artifact();
        *f.calls.on_query.lock().unwrap() =
            Some(Box::new(move || fs::write(path, b"bad").unwrap()));
        let out = f.run(&mut cache, mode);
        if mode == SearchMode::Auto {
            assert!(matches!(
                out.unwrap().fallback,
                Some(FallbackReason::ModelFilesInvalid { .. })
            ));
        } else {
            assert!(out
                .unwrap_err()
                .to_string()
                .contains("changed during retrieval"));
        }
        assert!(cache.entry.is_none());
        assert_eq!(f.factories(), 1);
        assert_eq!(f.calls.queries.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn lexical_preserves_warm_entry_without_model_reads_or_artifact_writes() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    let path = f.artifact();
    let before = Identity::read(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    f.store.set_meta("embedding.enabled", "false").unwrap();
    f.run(&mut cache, SearchMode::Lexical).unwrap();
    assert!(cache.entry.is_some());
    assert_eq!(f.factories(), 1);
    assert_eq!(f.calls.queries.load(Ordering::SeqCst), 1);
    assert_eq!(Identity::read(&path).unwrap(), before);
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn empty_filtered_semantic_branch_retains_successfully_loaded_runtime() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    let mut run = || {
        cache.search_with(
            &f.store,
            SearchQuery {
                text: "marker".into(),
                space_ids: Some(vec![]),
                limit: 5,
                at: None,
                include_retired: false,
                include_superseded: false,
            },
            SearchMode::Auto,
            f.dir.path(),
            lookup,
            &|entry, _| {
                f.calls.factories.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Runtime {
                    id: entry.id.into(),
                    calls: f.calls.clone(),
                }))
            },
            &|| f.now.get(),
            &|_| true,
        )
    };
    for _ in 0..2 {
        let out = run().unwrap();
        assert_eq!(out.fallback, Some(FallbackReason::NoSemanticCandidates));
    }
    assert!(cache.entry.is_some());
    assert_eq!(f.factories(), 1);
}

#[test]
fn malformed_model_setting_is_not_disabled_for_cached_or_uncached_readiness() {
    let f = Fixture::new();
    let mut cache = SemanticCache::default();
    f.run(&mut cache, SearchMode::Semantic).unwrap();
    assert_eq!(f.factories(), 1);

    f.store.set_meta("embedding.enabled", "false").unwrap();
    // Deliberate malformed fixture value, written through the actual M6 scope.
    // The old split reader returned Disabled before ever decoding this model.
    f.store
        .set_meta_blob_for_test("embedding.model", b"SYNTHETIC_INVALID_MODEL_TYPE")
        .unwrap();
    let uncached = super::super::resolve_with(&f.store, f.dir.path(), lookup, &|_, _| {
        panic!("malformed settings must not construct a runtime")
    })
    .unwrap_err();
    assert!(matches!(&uncached, Error::Storage(_)));
    assert!(uncached.to_string().to_lowercase().contains("blob"));

    for mode in [SearchMode::Auto, SearchMode::Semantic, SearchMode::Hybrid] {
        let error = f.run(&mut cache, mode).unwrap_err();
        assert!(matches!(&error, Error::Storage(_)));
        assert!(error.to_string().to_lowercase().contains("blob"));
        assert!(cache.entry.is_none());
    }
    assert_eq!(f.factories(), 1);
    assert_eq!(f.calls.queries.load(Ordering::SeqCst), 1);
    // Explicit lexical retrieval still bypasses semantic settings as before.
    assert!(!f
        .run(&mut cache, SearchMode::Lexical)
        .unwrap()
        .hits
        .is_empty());
}

fn shared_fixture() -> (tempfile::TempDir, Arc<SqliteStore>, Arc<Calls>) {
    let Fixture {
        dir, store, calls, ..
    } = Fixture::new();
    (dir, Arc::new(store), calls)
}

fn shared_query() -> SearchQuery {
    SearchQuery {
        text: "retrieval marker".into(),
        space_ids: None,
        limit: 5,
        at: None,
        include_superseded: false,
        include_retired: false,
    }
}

fn shared_factory<'a>(
    calls: &'a Arc<Calls>,
) -> impl Fn(&ModelManifestEntry, &Path) -> Result<Box<dyn EmbeddingRuntime>> + 'a {
    move |entry, _| {
        calls.factories.fetch_add(1, Ordering::SeqCst);
        if let Some(action) = calls.on_factory.lock().unwrap().take() {
            action();
        }
        Ok(Box::new(Runtime {
            id: entry.id.into(),
            calls: Arc::clone(calls),
        }) as Box<dyn EmbeddingRuntime>)
    }
}

type ModelWriters = Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>;

fn bounded_model_write(
    store: Arc<SqliteStore>,
    key: &'static str,
    value: &'static str,
    writers: ModelWriters,
) -> Box<dyn FnOnce() + Send> {
    Box::new(move || {
        let (sent, received) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let result = store.set_meta(key, value);
            let _ = sent.send(result);
        });
        writers.lock().unwrap().push(writer);
        received
            .recv_timeout(Duration::from_secs(2))
            .expect("model preparation must not retain the Store connection mutex")
            .unwrap();
    })
}

fn with_model_writers<T>(writers: &ModelWriters, operation: impl FnOnce() -> T) -> T {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
    for writer in writers.lock().unwrap().drain(..) {
        writer.join().unwrap();
    }
    match result {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[test]
fn model_factory_and_query_can_write_through_the_same_store_before_final_snapshot() {
    let (dir, store, calls) = shared_fixture();
    let writers = ModelWriters::default();
    *calls.on_factory.lock().unwrap() = Some(bounded_model_write(
        Arc::clone(&store),
        "fixture.factory",
        "SYNTHETIC",
        Arc::clone(&writers),
    ));
    *calls.on_query.lock().unwrap() = Some(bounded_model_write(
        Arc::clone(&store),
        "fixture.query",
        "SYNTHETIC",
        Arc::clone(&writers),
    ));
    let outcome = with_model_writers(&writers, || {
        crate::retrieval::search_with_preparer(
            store.as_ref(),
            shared_query(),
            SearchMode::Hybrid,
            None,
            |store| super::super::prepare_with(store, dir.path(), lookup, &shared_factory(&calls)),
        )
    })
    .unwrap();
    assert_eq!(outcome.effective, EffectiveMode::Hybrid);
    assert_eq!(calls.factories.load(Ordering::SeqCst), 1);
    assert_eq!(calls.queries.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.get_meta("fixture.factory").unwrap().as_deref(),
        Some("SYNTHETIC")
    );
    assert_eq!(
        store.get_meta("fixture.query").unwrap().as_deref(),
        Some("SYNTHETIC")
    );
}

#[test]
fn unavailable_preflight_is_revalidated_and_restarts_once_after_enablement() {
    for mode in [SearchMode::Auto, SearchMode::Semantic, SearchMode::Hybrid] {
        let (dir, store, calls) = shared_fixture();
        store.set_meta("embedding.enabled", "false").unwrap();
        let mut preparations = 0;
        let outcome = crate::retrieval::search_with_preparer(
            store.as_ref(),
            shared_query(),
            mode,
            None,
            |store| {
                preparations += 1;
                let prepared =
                    super::super::prepare_with(store, dir.path(), lookup, &shared_factory(&calls))?;
                if preparations == 1 {
                    store.set_meta("embedding.enabled", "true")?;
                }
                Ok(prepared)
            },
        )
        .unwrap();
        assert_eq!(preparations, 2);
        assert_eq!(
            outcome.effective,
            if mode == SearchMode::Semantic {
                EffectiveMode::Semantic
            } else {
                EffectiveMode::Hybrid
            }
        );
        assert_eq!(calls.factories.load(Ordering::SeqCst), 1);
        assert_eq!(calls.queries.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn uncached_profile_changes_twice_exhaust_exactly_one_restart() {
    let (dir, store, calls) = shared_fixture();
    let mut preparations = 0;
    let result = crate::retrieval::search_with_preparer(
        store.as_ref(),
        shared_query(),
        SearchMode::Hybrid,
        None,
        |store| {
            preparations += 1;
            let prepared =
                super::super::prepare_with(store, dir.path(), lookup, &shared_factory(&calls))?;
            store.set_meta(
                "embedding.model",
                if preparations == 1 {
                    OTHER.id
                } else {
                    MODEL.id
                },
            )?;
            Ok(prepared)
        },
    );
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("changed concurrently"));
    assert_eq!(preparations, 2);
    assert_eq!(calls.factories.load(Ordering::SeqCst), 2);
    assert_eq!(calls.queries.load(Ordering::SeqCst), 2);
    // Both final sessions ended before the caller retries or writes again.
    store
        .set_meta("fixture.after_exhaustion", "SYNTHETIC")
        .unwrap();
}

#[test]
fn cached_profile_change_restarts_once_and_does_not_reuse_stale_model() {
    let (dir, store, calls) = shared_fixture();
    let writers = ModelWriters::default();
    let mut cache = SemanticCache::default();
    let run = |cache: &mut SemanticCache| {
        cache.search_with(
            store.as_ref(),
            shared_query(),
            SearchMode::Hybrid,
            dir.path(),
            lookup,
            &shared_factory(&calls),
            &|| SystemTime::now() + Duration::from_secs(10),
            &|_| true,
        )
    };
    run(&mut cache).unwrap();
    *calls.on_query.lock().unwrap() = Some(bounded_model_write(
        Arc::clone(&store),
        "embedding.model",
        OTHER.id,
        Arc::clone(&writers),
    ));
    let outcome = with_model_writers(&writers, || run(&mut cache)).unwrap();
    assert_eq!(outcome.effective, EffectiveMode::Hybrid);
    assert_eq!(calls.factories.load(Ordering::SeqCst), 2);
    assert_eq!(calls.queries.load(Ordering::SeqCst), 3);
    assert_eq!(cache.entry.as_ref().unwrap().model.entry.id, OTHER.id);
}

#[test]
fn supplied_unavailable_is_explicit_policy_while_ready_capabilities_are_validated() {
    let (dir, store, calls) = shared_fixture();
    let deliberate = SemanticCapability::Unavailable(FallbackReason::EmbeddingsDisabled);
    let outcome = crate::retrieval::search_with_mode(
        store.as_ref(),
        shared_query(),
        SearchMode::Auto,
        &deliberate,
        RetrievalOptions::default(),
    )
    .unwrap();
    assert_eq!(outcome.effective, EffectiveMode::Lexical);
    assert_eq!(outcome.fallback, Some(FallbackReason::EmbeddingsDisabled));
    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        assert!(matches!(
            crate::retrieval::search_with_mode(
                store.as_ref(),
                shared_query(),
                mode,
                &deliberate,
                RetrievalOptions::default()
            ),
            Err(Error::SemanticUnavailable { .. })
        ));
    }
    let ready =
        super::super::resolve_with(store.as_ref(), dir.path(), lookup, &shared_factory(&calls))
            .unwrap();
    store.set_meta("embedding.model", OTHER.id).unwrap();
    assert!(crate::retrieval::search_with_mode(
        store.as_ref(),
        shared_query(),
        SearchMode::Semantic,
        &ready,
        RetrievalOptions::default()
    )
    .unwrap_err()
    .to_string()
    .contains("changed concurrently"));
    store
        .set_meta_blob_for_test("embedding.model", b"SYNTHETIC_BLOB")
        .unwrap();
    assert!(crate::retrieval::search_with_mode(
        store.as_ref(),
        shared_query(),
        SearchMode::Lexical,
        &ready,
        RetrievalOptions::default()
    )
    .is_ok());
}

#[test]
fn cached_profile_change_twice_exhausts_one_restart_and_evicts_runtime() {
    let (dir, store, calls) = shared_fixture();
    let writers = ModelWriters::default();
    let mut cache = SemanticCache::default();
    let factory = |entry: &ModelManifestEntry, _: &Path| -> Result<Box<dyn EmbeddingRuntime>> {
        let attempt = calls.factories.fetch_add(1, Ordering::SeqCst);
        bounded_model_write(
            Arc::clone(&store),
            "embedding.model",
            if attempt == 0 { OTHER.id } else { MODEL.id },
            Arc::clone(&writers),
        )();
        Ok(Box::new(Runtime {
            id: entry.id.into(),
            calls: Arc::clone(&calls),
        }))
    };
    let result = with_model_writers(&writers, || {
        cache.search_with(
            store.as_ref(),
            shared_query(),
            SearchMode::Hybrid,
            dir.path(),
            lookup,
            &factory,
            &|| SystemTime::now() + Duration::from_secs(10),
            &|_| true,
        )
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("changed concurrently"));
    assert_eq!(calls.factories.load(Ordering::SeqCst), 2);
    assert_eq!(calls.queries.load(Ordering::SeqCst), 2);
    assert!(cache.entry.is_none());
    store
        .set_meta("fixture.after_cached_exhaustion", "SYNTHETIC")
        .unwrap();
}
