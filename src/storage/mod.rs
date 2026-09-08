pub mod sqlite;

use uuid::Uuid;

use crate::core::{
    ConflictGroup, IngestBundle, IngestRecord, Memory, MemoryDetail, MemorySummary, Result,
    SearchHitRaw, SearchQuery, Source, SourceKind, Space, SpaceLink, Ts,
};

/// The storage boundary. Methods correspond to use cases, not tables, and
/// traffic exclusively in `core` types. Multi-invariant operations are
/// single methods that own their transaction (`record_ingestion`); a future
/// PostgreSQL backend would implement this trait without any other change.
pub trait Store {
    // -- spaces -------------------------------------------------------------

    fn create_space(&self, name: &str, description: &str) -> Result<Space>;
    fn get_space(&self, name: &str) -> Result<Space>;
    fn list_spaces(&self) -> Result<Vec<Space>>;
    /// Spaces with a count of their active memories, ordered by name.
    fn list_spaces_with_counts(&self) -> Result<Vec<(Space, u64)>>;
    fn list_space_links(&self) -> Result<Vec<SpaceLink>>;
    fn link_spaces(&self, from: Uuid, to: Uuid) -> Result<SpaceLink>;

    // -- ingestion ----------------------------------------------------------

    fn find_source(
        &self,
        kind: SourceKind,
        location: &str,
        content_hash: &str,
    ) -> Result<Option<Source>>;
    /// Atomically records one ingestion: source, evidence, assertions, FTS
    /// index entries, memories, space memberships, and the reconciliation
    /// against active memories at the same location (supersessions and
    /// retirements), exactly as specified in docs/memory-model.md 6.2.
    fn record_ingestion(&self, bundle: IngestBundle) -> Result<IngestRecord>;

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
