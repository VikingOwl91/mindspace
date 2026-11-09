pub mod sqlite;

use uuid::Uuid;

use crate::core::{
    AssertionEmbedding, CodeDerivation, CodeHit, CodeLanguage, CodeObservation,
    CodeObservationReport, CodeParserIdentity, CodeResolutionReport, CodeSearchQuery,
    ConflictGroup, EmbeddingIndexStatus, GitObservation, GitObservationReport, GitRefObservation,
    IngestObservation, ManualMemoryDraft, ManualMemoryReport, Memory, MemoryDetail, MemorySummary,
    ObservationReport, Result, SearchHitRaw, SearchQuery, SourceLifecycle, Space, SpaceLink,
    TemporalDimension, TemporalHistoryCapabilities, Ts, UnembeddedAssertion,
};

/// The storage boundary. Methods correspond to use cases, not tables, and
/// traffic exclusively in `core` types. Each state-changing method owns its
/// transaction; a future PostgreSQL backend would implement this trait
/// without exposing transaction handles to callers.
pub trait Store {
    fn temporal_history_capabilities(&self) -> Result<TemporalHistoryCapabilities>;
    fn require_temporal_coverage(
        &self,
        revision: i64,
        dimensions: &[TemporalDimension],
    ) -> Result<()>;

    // -- spaces -------------------------------------------------------------

    fn create_space(&self, name: &str, description: &str) -> Result<Space>;
    fn get_space(&self, name: &str) -> Result<Space>;
    fn list_spaces(&self) -> Result<Vec<Space>>;
    /// Spaces with a count of their active memories, ordered by name.
    fn list_spaces_with_counts(&self) -> Result<Vec<(Space, u64)>>;
    fn list_space_links(&self) -> Result<Vec<SpaceLink>>;
    fn link_spaces(&self, from: Uuid, to: Uuid) -> Result<SpaceLink>;

    // -- observation / ingestion --------------------------------------------

    /// Atomically records one ingest operation: the session, per-file
    /// observations, source identity and lifecycle decisions (unchanged,
    /// new version, inferred move, new logical source, disappearance), new
    /// versions with their evidence, and the memory reconciliation for
    /// changed logical sources, exactly as specified in
    /// docs/memory-model.md section 6.
    fn record_observation(&self, observation: IngestObservation) -> Result<ObservationReport>;
    /// The logical identity and lifecycle journal of one source.
    fn get_source_lifecycle(&self, logical_id: Uuid) -> Result<Option<SourceLifecycle>>;

    /// Atomically records one git repository observation: the session,
    /// repository identity, all newly discovered immutable objects, the
    /// latest ref states plus their journal events, and the derivation of
    /// text blobs at ref tips through the normal evidence/assertion
    /// pipeline. Counters in the report are deterministic.
    fn record_git_observation(&self, observation: GitObservation) -> Result<GitObservationReport>;
    /// Latest observed ref states of one repository, ordered by name.
    fn git_refs_current(&self, repository_id: Uuid) -> Result<Vec<GitRefObservation>>;
    fn record_code_observation(
        &self,
        observation: CodeObservation,
    ) -> Result<CodeObservationReport>;
    /// Atomically records one directory ingestion: the observation and, if there is any, the
    /// structure derived from its source files. Either everything commits (as one recording
    /// revision) or nothing does. `report.code` carries the code counters.
    ///
    /// `before_commit` runs after every write succeeded and immediately before the COMMIT, still
    /// inside the transaction. If it fails the transaction is rolled back and its error is returned:
    /// nothing is committed. (This is the hook for a commit-time authorization decision.)
    fn record_files_ingest(
        &self,
        observation: IngestObservation,
        code: Option<CodeObservation>,
        before_commit: &dyn Fn() -> Result<()>,
    ) -> Result<ObservationReport>;
    fn code_structure(&self, target: &str) -> Result<Vec<CodeDerivation>>;
    /// Symbol search over the code-structure layer: definitions whose name,
    /// qualified name, signature, or path match every query term, ranked by
    /// match quality. Read-only; by default only current definitions.
    fn search_code(&self, query: &CodeSearchQuery) -> Result<Vec<CodeHit>>;
    fn has_code_derivation(
        &self,
        language: CodeLanguage,
        content_hash: &str,
        repository_id: Option<Uuid>,
        blob_oid: Option<&str>,
        identity: &CodeParserIdentity,
    ) -> Result<bool>;
    fn resolve_git_code_relationships(
        &self,
        repository_id: Uuid,
        commit_oid: &str,
        resolver: &str,
        version: &str,
    ) -> Result<CodeResolutionReport>;

    /// Atomically creates or reuses an explicitly written memory. Manual
    /// writes receive their own source/evidence provenance and never claim
    /// to originate from an ingested document.
    fn record_manual_write(&self, draft: ManualMemoryDraft) -> Result<ManualMemoryReport>;
    /// Adds memberships to an active memory and returns newly added space
    /// names. Existing membership is idempotent.
    fn add_memory_spaces(&self, memory_id: Uuid, space_ids: Vec<Uuid>) -> Result<Vec<String>>;

    // -- embeddings / vector index (Phase 2 M1) -----------------------------
    // Derived, versioned index over immutable assertions. Storage owns the
    // BLOB format and similarity computation; it never runs a model. The
    // derivation key identifies the document pipeline, so a change to any of
    // its parameters invalidates prior rows (they are ignored, not deleted,
    // and re-indexed by the caller). The query prefix is not part of the key:
    // the runtime applies it at query time, and the manifest entry pins it.

    /// Idempotently store derived embeddings. Rows are keyed by
    /// (assertion_id, derivation_key); an existing row is left unchanged.
    /// Returns the number of rows newly inserted.
    fn save_embeddings(&self, embeddings: &[AssertionEmbedding]) -> Result<usize>;

    /// Assertions that have no embedding for the given derivation key, up to
    /// `limit`. Ordered by created_at then id for determinism. Used by the
    /// indexer for incremental backfill; an empty result means full coverage.
    fn list_assertions_needing_embedding(
        &self,
        derivation_key: &str,
        after: Option<(Ts, Uuid)>,
        limit: u32,
    ) -> Result<Vec<UnembeddedAssertion>>;

    /// Coverage counts for one derivation key over the assertion population.
    fn embedding_index_status(&self, derivation_key: &str) -> Result<EmbeddingIndexStatus>;

    /// Remove all embeddings for one derivation key (rebuild / model switch).
    /// Returns the number of rows removed.
    fn clear_embeddings(&self, derivation_key: &str) -> Result<usize>;

    /// Read a database-level setting.
    fn get_meta(&self, key: &str) -> Result<Option<String>>;
    /// Write a database-level setting.
    fn set_meta(&self, key: &str, value: &str) -> Result<()>;

    /// Vector similarity search over assertions, applying the same space,
    /// temporal, and lifecycle filters as `search`. Returns hits ranked by
    /// cosine similarity (higher = better) as SearchHitRaw, with space_names
    /// filled in post-hoc. All filter-qualified candidates are evaluated
    /// before ranking; query.limit is applied only after ranking, so the
    /// top-N is never drawn from an incomplete candidate subset.
    fn search_vectors(
        &self,
        query_vector: &[f32],
        query: &SearchQuery,
        derivation_key: &str,
    ) -> Result<Vec<SearchHitRaw>>;

    // -- memory inspection ---------------------------------------------------

    fn get_memory_detail(&self, id: Uuid) -> Result<Option<MemoryDetail>>;
    /// All memories (any state) with this normalized subject, oldest first.
    fn list_memories_by_subject(&self, subject_normalized: &str) -> Result<Vec<MemorySummary>>;
    /// Memories that were superseded by `id`.
    fn list_replaced_by(&self, id: Uuid) -> Result<Vec<MemorySummary>>;
    /// Memory ids starting with `prefix`.
    fn memory_id_prefix_matches(&self, prefix: &str) -> Result<Vec<Uuid>>;

    // -- retrieval -----------------------------------------------------------

    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHitRaw>>;
    fn list_conflicts(&self) -> Result<Vec<ConflictGroup>>;

    // -- lifecycle -----------------------------------------------------------

    fn retire_memory(&self, id: Uuid, reason: Option<String>, at: Ts) -> Result<Memory>;
}
