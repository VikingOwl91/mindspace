pub mod sqlite;

use uuid::Uuid;

use crate::core::{
    Anchor, AssertionEmbedding, CodeDerivation, CodeHit, CodeLanguage, CodeObservation,
    CodeObservationReport, CodeParserIdentity, CodeResolutionReport, CodeSearchQuery,
    ConflictGroup, DocumentExtractionDraft, DocumentExtractionInfo, DocumentExtractionReport,
    EmbeddingIndexStatus, GitObservation, GitObservationReport, GitRefObservation,
    IngestObservation, ManualMemoryDraft, ManualMemoryReport, Memory, MemoryDetail, MemorySummary,
    ObservationReport, Result, SearchHitRaw, SearchQuery, SourceLifecycle, Space, SpaceLink,
    StoredCitation, TemporalDimension, TemporalHistoryCapabilities, Ts, UnembeddedAssertion,
};

/// The relationship resolutions that belong to one Git ingestion: run for each commit after the
/// ingestion's code structure was recorded.
#[derive(Debug, Clone, Copy)]
pub struct GitResolutionPlan<'a> {
    pub commits: &'a [String],
    pub resolver: &'a str,
    pub version: &'a str,
}

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
    // -- documents (M8 S1) --------------------------------------------------

    /// Atomically records one document extraction: the document (a logical source of kind
    /// `document`, one per location), its revision (the canonical text), the extraction keyed by the
    /// original-bytes hash and the extractor identity, its structure nodes and, only if the draft
    /// asks for it, the original bytes. All or nothing, in one recording revision; an identical
    /// repeat changes nothing and consumes no revision. It never touches memories, assertions,
    /// evidence or the file lifecycle.
    fn record_document_extraction(
        &self,
        draft: DocumentExtractionDraft,
    ) -> Result<DocumentExtractionReport>;
    /// Explicit document chunking. Validates the actual tokenizer budget before one atomic
    /// extraction + chunk-set write; never creates assertions or memories.
    #[cfg(feature = "embeddings")]
    fn record_chunked_document(
        &self,
        draft: DocumentExtractionDraft,
        budget: &crate::embeddings::budget::TokenBudget,
    ) -> Result<DocumentExtractionReport>;
    /// The stored side of a citation: `None` when no extraction with the anchor's identity exists
    /// (wrong revision, other extractor, unknown document). An anchor that names more than the stored
    /// extraction supports is an error, never a downgrade.
    fn resolve_document_anchor(&self, anchor: &Anchor) -> Result<Option<StoredCitation>>;
    /// Every extraction of one document, oldest first.
    fn list_document_extractions(&self, logical_id: Uuid) -> Result<Vec<DocumentExtractionInfo>>;
    /// Erases retained original bytes. The extractions, their text and their anchors stay; their
    /// citations simply stop reporting `bytes_resolved`. `false`: nothing was retained.
    fn erase_document_bytes(&self, source_bytes_hash: &str) -> Result<bool>;

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
    ///
    /// Error contract: `Error::CommitOutcomeUnknown` is the ONLY error after which the data may
    /// have been committed (the COMMIT itself failed in a way that does not say). Every other
    /// error means nothing was committed and no recording revision was consumed.
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
    /// Atomically records one logical Git ingestion: the git observation, then (when there is any)
    /// the code structure derived from its blobs, then one relationship resolution per commit of
    /// `resolutions`, which reads the rows the code write just made. Everything commits as ONE
    /// transaction and ONE recording revision, or nothing does. `report.code` carries the code counters.
    ///
    /// Error contract and `before_commit`: as for [`Store::record_files_ingest`].
    fn record_git_ingest(
        &self,
        observation: GitObservation,
        code: Option<CodeObservation>,
        resolutions: GitResolutionPlan<'_>,
        before_commit: &dyn Fn() -> Result<()>,
    ) -> Result<GitObservationReport>;
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

/// One immutable, revision-bound document chunk. `context` names structure nodes only; it is
/// never copied into the cited source range. Retrieval projection is deliberately deferred.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDocumentChunk {
    pub id: String,
    pub extraction_id: crate::core::Uuid,
    pub version: String,
    pub budget_identity: String,
    pub start: u64,
    pub end: u64,
    pub tokens: usize,
    pub context: Vec<Vec<u32>>,
    pub anchor: crate::core::Anchor,
}
