pub mod sqlite;

use uuid::Uuid;

use crate::core::{
    CodeDerivation, CodeLanguage, CodeObservation, CodeObservationReport, CodeParserIdentity,
    ConflictGroup, GitObservation, GitObservationReport, GitRefObservation, IngestObservation,
    Memory, MemoryDetail, MemorySummary, ObservationReport, Result, SearchHitRaw, SearchQuery,
    SourceLifecycle, Space, SpaceLink, Ts,
};

/// The storage boundary. Methods correspond to use cases, not tables, and
/// traffic exclusively in `core` types. Multi-invariant operations are
/// single methods that own their transaction (`record_observation`); a
/// future PostgreSQL backend would implement this trait without any other
/// change.
pub trait Store {
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
    fn code_structure(&self, target: &str) -> Result<Vec<CodeDerivation>>;
    fn has_code_derivation(
        &self,
        language: CodeLanguage,
        content_hash: &str,
        repository_id: Option<Uuid>,
        blob_oid: Option<&str>,
        identity: &CodeParserIdentity,
    ) -> Result<bool>;

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
