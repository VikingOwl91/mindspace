//! Search orchestration.
//!
//! [`search`] is the lexical search existing callers use. It runs the store's
//! lexical query and decorates hits with directed space traversal. Its output
//! does not depend on the modes below.
//!
//! [`search_with_mode`] adds explicit retrieval modes. It reports the mode that
//! served the results, fuses the lexical and semantic branches with reciprocal
//! rank fusion (see [`fusion`]), and never compares score kinds. The semantic
//! capability comes from [`semantic`], which reads metadata, verifies local
//! artifacts, and constructs a lazy runtime but never downloads or indexes.
//!
//! Traversal follows `from -> to` links only ("project:owlry -> coding" never
//! implies "coding -> project:owlry") and is cycle-safe through a visited set.
//! Both branches of one search share a single expanded scope, so the space,
//! lifecycle, and temporal filters are identical for each.

pub mod fusion;
pub mod semantic;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

use serde::Serialize;

use crate::core::{
    Error, Result, SearchHit, SearchHitRaw, SearchQuery, Space, SpaceLink, SpaceMatch, Uuid,
};
use crate::storage::Store;

pub use semantic::{SemanticCapability, SemanticSource};

/// The default reciprocal-rank-fusion constant `k`.
pub const RRF_K_DEFAULT: NonZeroU32 = NonZeroU32::new(60).unwrap();

const POOL_MIN: u32 = 20;
const POOL_MAX: u32 = 200;

/// The retrieval mode a caller requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Deterministic full-text search. The default. Never touches embeddings.
    #[default]
    Lexical,
    /// Query embedding followed by vector similarity. Fails when unavailable.
    Semantic,
    /// Lexical and semantic branches fused with RRF. Fails when unavailable.
    Hybrid,
    /// Hybrid when the semantic branch is usable and returns candidates.
    /// Otherwise lexical, with the reason recorded in the outcome.
    Auto,
}

impl SearchMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
            Self::Hybrid => "hybrid",
            Self::Auto => "auto",
        }
    }
}

impl FromStr for SearchMode {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "lexical" => Ok(Self::Lexical),
            "semantic" => Ok(Self::Semantic),
            "hybrid" => Ok(Self::Hybrid),
            "auto" => Ok(Self::Auto),
            _ => Err(format!(
                "unknown retrieval mode '{value}'; expected lexical, semantic, hybrid, or auto"
            )),
        }
    }
}

/// The mode whose results an outcome carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveMode {
    Lexical,
    Semantic,
    Hybrid,
    /// No branch ran, typically because the caller requested a zero limit.
    NotRun,
}

impl EffectiveMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
            Self::Hybrid => "hybrid",
            Self::NotRun => "not_run",
        }
    }
}

/// What a hit's `score` measures. Values of different kinds are never
/// comparable, so read a score only together with its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreKind {
    /// Negated BM25 from the lexical index. Higher is better.
    Lexical,
    /// Cosine similarity between the query vector and the memory vector.
    Semantic,
    /// Reciprocal rank fusion over the candidate pools. Meaningful only within
    /// one search.
    Fused,
}

impl ScoreKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
            Self::Fused => "fused",
        }
    }
}

/// Why a requested capability was not used. The display text names the
/// command that fixes the problem where one exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FallbackReason {
    EmbeddingsDisabled,
    NoModelSelected,
    UnknownModel { id: String },
    FeatureDisabled,
    ModelFilesMissing { model: String },
    ModelFilesInvalid { model: String, detail: String },
    NoCompatibleIndex,
    RuntimeUnavailable { detail: String },
    QueryEmbeddingFailed { detail: String },
    NoSemanticCandidates,
}

impl fmt::Display for FallbackReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmbeddingsDisabled => {
                write!(f, "embeddings are disabled; run: mind embeddings enable")
            }
            Self::NoModelSelected => write!(
                f,
                "no embedding model is selected; install one with: mind model install <id>, select it with: mind model use <id> (models: mind model list)"
            ),
            Self::UnknownModel { id } => write!(
                f,
                "selected embedding model '{id}' is not a supported model; run: mind model list"
            ),
            Self::FeatureDisabled => write!(
                f,
                "this build has no embedding support; rebuild with --features embeddings"
            ),
            Self::ModelFilesMissing { model } => write!(
                f,
                "files for model '{model}' are missing; run: mind model install {model}"
            ),
            Self::ModelFilesInvalid { model, detail } => write!(
                f,
                "files for model '{model}' failed verification ({detail}); run: mind model install {model}"
            ),
            Self::NoCompatibleIndex => write!(
                f,
                "no vectors are indexed for the selected model; run: mind index run"
            ),
            Self::RuntimeUnavailable { detail } => {
                write!(f, "embedding runtime unavailable: {detail}")
            }
            Self::QueryEmbeddingFailed { detail } => {
                write!(f, "query embedding failed: {detail}")
            }
            Self::NoSemanticCandidates => {
                write!(f, "no semantic candidates matched the query filters")
            }
        }
    }
}

/// One hit from [`search_with_mode`]. `hit.score` holds the value named by
/// `score_kind`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RetrievalHit {
    pub hit: SearchHit,
    pub score_kind: ScoreKind,
    pub provenance: Provenance,
}

/// How a hit was found. In Hybrid, ranks and branch scores describe the first
/// ranked eligible episode representing the assertion in each branch.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Provenance {
    pub lexical_rank: Option<u32>,
    pub semantic_rank: Option<u32>,
    /// Negated BM25 from the lexical branch, when that branch returned the assertion.
    pub lexical_score: Option<f64>,
    /// Cosine similarity from the semantic branch, when that branch returned the assertion.
    pub semantic_score: Option<f64>,
}

/// The result of [`search_with_mode`]. `requested` is what the caller asked
/// for and `effective` is what produced `hits`; `NotRun` means no branch ran.
/// `fallback` says why an Auto or Hybrid request was served lexically.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchOutcome {
    pub requested: SearchMode,
    pub effective: EffectiveMode,
    pub fallback: Option<FallbackReason>,
    /// Candidates returned by the lexical branch. Fusion counts assertions
    /// after deduplication; lexical paths count memory episodes. A
    /// `NoSemanticCandidates` fallback reports its pool-sized episode input.
    pub lexical_candidates: usize,
    /// Candidates returned by the semantic branch. Fusion counts assertions
    /// after deduplication; Semantic mode counts memory episodes.
    pub semantic_candidates: usize,
    pub hits: Vec<RetrievalHit>,
}

/// Tuning for [`search_with_mode`]. The type enforces that `rrf_k` is at least 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrievalOptions {
    pub rrf_k: NonZeroU32,
    /// Candidates drawn from each branch before fusion. `None` uses
    /// [`candidate_pool_size`]. Values below the limit are raised to the limit.
    pub candidate_pool: Option<u32>,
}

impl Default for RetrievalOptions {
    fn default() -> Self {
        Self {
            rrf_k: RRF_K_DEFAULT,
            candidate_pool: None,
        }
    }
}

/// Candidates drawn from each branch before fusion. Never below the limit.
/// Otherwise four times the limit, clamped to 20..=200, so small queries see
/// enough candidates and large limits do not widen the vector scan. A zero
/// limit has an empty pool.
pub fn candidate_pool_size(limit: u32, override_pool: Option<u32>) -> u32 {
    if limit == 0 {
        return 0;
    }
    match override_pool {
        Some(pool) => pool.max(limit),
        None => limit.max(limit.saturating_mul(4).clamp(POOL_MIN, POOL_MAX)),
    }
}

/// Lexical search with directed space traversal. This is the path existing
/// callers use, and its output is unchanged.
pub fn search(store: &dyn Store, query: SearchQuery) -> Result<Vec<SearchHit>> {
    let (scoped, traversal) = expand_scope(store, query)?;
    let raw = store.search(&scoped)?;
    Ok(raw
        .into_iter()
        .map(|hit| {
            let score = hit.score;
            decorate(hit, traversal.as_ref(), score)
        })
        .collect())
}

/// Resolves the current semantic capability once for an explicit mode request,
/// then delegates to [`search_with_mode`]. Lexical requests never inspect model
/// metadata or artifacts. Blank queries fail before capability resolution.
pub fn search_with_mode_resolving_capability(
    store: &dyn Store,
    query: SearchQuery,
    mode: SearchMode,
) -> Result<SearchOutcome> {
    search_with_mode_and_resolver(store, query, mode, semantic::resolve)
}

fn search_with_mode_and_resolver(
    store: &dyn Store,
    query: SearchQuery,
    mode: SearchMode,
    resolver: impl FnOnce(&dyn Store) -> Result<SemanticCapability>,
) -> Result<SearchOutcome> {
    if query.text.trim().is_empty() {
        return Err(Error::EmptyQuery);
    }
    let capability = if mode == SearchMode::Lexical {
        SemanticCapability::Unavailable(FallbackReason::EmbeddingsDisabled)
    } else {
        resolver(store)?
    };
    search_with_mode(store, query, mode, &capability, RetrievalOptions::default())
}

/// Runs `query` in `mode`.
///
/// `semantic` is the capability resolved for this call (see
/// [`semantic::resolve`]). Lexical mode ignores it. Semantic and Hybrid fail
/// with [`Error::SemanticUnavailable`] when it is unavailable or the query
/// cannot be embedded. Auto degrades to lexical and records the reason.
///
/// Storage and traversal errors always propagate. Blank query text is rejected
/// in every mode. The result never holds more than `query.limit` hits.
pub fn search_with_mode(
    store: &dyn Store,
    query: SearchQuery,
    mode: SearchMode,
    semantic: &SemanticCapability,
    options: RetrievalOptions,
) -> Result<SearchOutcome> {
    let text = query.text.trim().to_string();
    if text.is_empty() {
        return Err(Error::EmptyQuery);
    }
    let plan = choose_plan(mode, semantic)?;
    let limit = query.limit;
    if limit == 0 {
        return Ok(SearchOutcome {
            requested: mode,
            effective: EffectiveMode::NotRun,
            fallback: None,
            lexical_candidates: 0,
            semantic_candidates: 0,
            hits: Vec::new(),
        });
    }

    // One expansion serves both branches, so their filters cannot diverge.
    let (scoped, traversal) = expand_scope(store, query)?;
    let traversal = traversal.as_ref();
    match plan {
        Plan::Lexical { fallback } => {
            lexical_outcome(store, mode, &scoped, traversal, limit, fallback)
        }
        Plan::Semantic(source) => {
            let vector = source
                .embed_query(&text)
                .map_err(|reason| unavailable(&reason))?;
            let raw = dedupe_hits(store.search_vectors(
                &vector,
                &with_limit(&scoped, limit),
                source.derivation_key(),
            )?);
            Ok(SearchOutcome {
                requested: mode,
                effective: EffectiveMode::Semantic,
                fallback: None,
                lexical_candidates: 0,
                semantic_candidates: raw.len(),
                hits: semantic_hits(raw, traversal, limit),
            })
        }
        Plan::Fusion { source, auto } => {
            let vector = match source.embed_query(&text) {
                Ok(vector) => vector,
                Err(reason) if auto => {
                    return lexical_outcome(store, mode, &scoped, traversal, limit, Some(reason));
                }
                Err(reason) => return Err(unavailable(&reason)),
            };
            let pool = candidate_pool_size(limit, options.candidate_pool);
            let lexical_episodes = dedupe_hits(store.search(&with_limit(&scoped, pool))?);
            let semantic_episodes = dedupe_hits(store.search_vectors(
                &vector,
                &with_limit(&scoped, pool),
                source.derivation_key(),
            )?);
            if semantic_episodes.is_empty() {
                // The vector branch contributed nothing. The lexical ranking is
                // the honest result, so it is reported as lexical and keeps
                // legacy episode-level behavior.
                return Ok(SearchOutcome {
                    requested: mode,
                    effective: EffectiveMode::Lexical,
                    fallback: Some(FallbackReason::NoSemanticCandidates),
                    lexical_candidates: lexical_episodes.len(),
                    semantic_candidates: 0,
                    hits: lexical_hits(lexical_episodes, traversal, limit),
                });
            }
            let lexical = dedupe_assertions(lexical_episodes);
            let semantic = dedupe_assertions(semantic_episodes);
            Ok(fused_outcome(
                mode, lexical, semantic, traversal, limit, options,
            ))
        }
    }
}

/// What a search will run. Decided before any branch executes.
enum Plan<'a> {
    Lexical {
        fallback: Option<FallbackReason>,
    },
    Semantic(&'a SemanticSource),
    Fusion {
        source: &'a SemanticSource,
        auto: bool,
    },
}

fn choose_plan(mode: SearchMode, semantic: &SemanticCapability) -> Result<Plan<'_>> {
    Ok(match (mode, semantic) {
        (SearchMode::Lexical, _) => Plan::Lexical { fallback: None },
        (SearchMode::Auto, SemanticCapability::Unavailable(reason)) => Plan::Lexical {
            fallback: Some(reason.clone()),
        },
        (SearchMode::Semantic, SemanticCapability::Ready(source)) => Plan::Semantic(source),
        (SearchMode::Hybrid, SemanticCapability::Ready(source)) => Plan::Fusion {
            source,
            auto: false,
        },
        (SearchMode::Auto, SemanticCapability::Ready(source)) => {
            Plan::Fusion { source, auto: true }
        }
        (SearchMode::Semantic | SearchMode::Hybrid, SemanticCapability::Unavailable(reason)) => {
            return Err(unavailable(reason))
        }
    })
}

fn unavailable(reason: &FallbackReason) -> Error {
    Error::SemanticUnavailable {
        reason: reason.to_string(),
    }
}

fn lexical_outcome(
    store: &dyn Store,
    mode: SearchMode,
    scoped: &SearchQuery,
    traversal: Option<&Traversal>,
    limit: u32,
    fallback: Option<FallbackReason>,
) -> Result<SearchOutcome> {
    let raw = dedupe_hits(store.search(&with_limit(scoped, limit))?);
    Ok(SearchOutcome {
        requested: mode,
        effective: EffectiveMode::Lexical,
        fallback,
        lexical_candidates: raw.len(),
        semantic_candidates: 0,
        hits: lexical_hits(raw, traversal, limit),
    })
}

fn fused_outcome(
    mode: SearchMode,
    lexical: Vec<SearchHitRaw>,
    semantic: Vec<SearchHitRaw>,
    traversal: Option<&Traversal>,
    limit: u32,
    options: RetrievalOptions,
) -> SearchOutcome {
    let lexical_candidates = lexical.len();
    let semantic_candidates = semantic.len();
    let lexical_ids: Vec<Uuid> = lexical.iter().map(|hit| hit.assertion_id).collect();
    let semantic_ids: Vec<Uuid> = semantic.iter().map(|hit| hit.assertion_id).collect();
    let lexical_scores: HashMap<Uuid, f64> = lexical
        .iter()
        .map(|hit| (hit.assertion_id, hit.score))
        .collect();
    let semantic_scores: HashMap<Uuid, f64> = semantic
        .iter()
        .map(|hit| (hit.assertion_id, hit.score))
        .collect();

    // Each assertion is shown from one eligible memory episode per branch.
    // The lexical row wins where both branches returned it, so the FTS
    // snippet is kept. Branch deduplication retained the first ranked episode.
    let mut payloads: HashMap<Uuid, SearchHitRaw> = semantic
        .into_iter()
        .map(|hit| (hit.assertion_id, hit))
        .collect();
    payloads.extend(lexical.into_iter().map(|hit| (hit.assertion_id, hit)));

    let hits = fusion::rrf_fuse(&lexical_ids, &semantic_ids, options.rrf_k, limit as usize)
        .into_iter()
        .filter_map(|fused| {
            let raw = payloads.remove(&fused.assertion_id)?;
            Some(RetrievalHit {
                hit: decorate(raw, traversal, fused.score),
                score_kind: ScoreKind::Fused,
                provenance: Provenance {
                    lexical_rank: fused.lexical_rank,
                    semantic_rank: fused.semantic_rank,
                    lexical_score: lexical_scores.get(&fused.assertion_id).copied(),
                    semantic_score: semantic_scores.get(&fused.assertion_id).copied(),
                },
            })
        })
        .collect();

    SearchOutcome {
        requested: mode,
        effective: EffectiveMode::Hybrid,
        fallback: None,
        lexical_candidates,
        semantic_candidates,
        hits,
    }
}

fn lexical_hits(
    raw: Vec<SearchHitRaw>,
    traversal: Option<&Traversal>,
    limit: u32,
) -> Vec<RetrievalHit> {
    raw.into_iter()
        .take(limit as usize)
        .enumerate()
        .map(|(index, hit)| {
            let score = hit.score;
            RetrievalHit {
                hit: decorate(hit, traversal, score),
                score_kind: ScoreKind::Lexical,
                provenance: Provenance {
                    lexical_rank: Some(fusion::rank_at(index)),
                    semantic_rank: None,
                    lexical_score: Some(score),
                    semantic_score: None,
                },
            }
        })
        .collect()
}

fn semantic_hits(
    raw: Vec<SearchHitRaw>,
    traversal: Option<&Traversal>,
    limit: u32,
) -> Vec<RetrievalHit> {
    raw.into_iter()
        .take(limit as usize)
        .enumerate()
        .map(|(index, hit)| {
            let score = hit.score;
            RetrievalHit {
                hit: decorate(hit, traversal, score),
                score_kind: ScoreKind::Semantic,
                provenance: Provenance {
                    lexical_rank: None,
                    semantic_rank: Some(fusion::rank_at(index)),
                    lexical_score: None,
                    semantic_score: Some(score),
                },
            }
        })
        .collect()
}

fn with_limit(query: &SearchQuery, limit: u32) -> SearchQuery {
    SearchQuery {
        limit,
        ..query.clone()
    }
}

/// Keeps the first hit per memory. The store already returns unique rows; this
/// keeps the one-row-per-memory invariant local to fusion.
fn dedupe_hits(raw: Vec<SearchHitRaw>) -> Vec<SearchHitRaw> {
    let mut seen = HashSet::new();
    raw.into_iter()
        .filter(|hit| seen.insert(hit.memory_id))
        .collect()
}

/// Keeps the first (best-ranked) eligible memory episode for each assertion.
/// Called only for branches entering RRF, after storage has applied all query
/// filters and deterministic ordering.
fn dedupe_assertions(raw: Vec<SearchHitRaw>) -> Vec<SearchHitRaw> {
    let mut seen = HashSet::new();
    raw.into_iter()
        .filter(|hit| seen.insert(hit.assertion_id))
        .collect()
}

/// The expanded scope of one search: the requested roots, every reachable space
/// with its path, and the names needed to explain each hit.
struct Traversal {
    roots: Vec<Uuid>,
    parents: HashMap<Uuid, Option<Uuid>>,
    id_to_name: HashMap<Uuid, String>,
    spaces: Vec<Space>,
}

impl Traversal {
    /// Keeps the memberships visible from a root and explains indirect ones.
    fn matches(&self, names: Vec<String>) -> Vec<SpaceMatch> {
        let mut matches = Vec::new();
        for name in names {
            // Names are unique, so the name identifies the space.
            let Some(sid) = self.spaces.iter().find(|s| s.name == name).map(|s| s.id) else {
                continue;
            };
            if self.roots.contains(&sid) {
                matches.push(SpaceMatch { name, via: None });
            } else if let Some(path) = path_from_root(sid, &self.parents, &self.id_to_name) {
                matches.push(SpaceMatch {
                    name,
                    via: Some(path[1..].join(" -> ")),
                });
            }
            // Spaces not reachable from any root stay out of the filtered view;
            // the memory still exists in them.
        }
        matches
    }
}

/// Expands the requested spaces to everything reachable from them, once. The
/// returned query carries the expanded set, and both branches of a search use it.
fn expand_scope(
    store: &dyn Store,
    mut query: SearchQuery,
) -> Result<(SearchQuery, Option<Traversal>)> {
    let roots: Vec<Uuid> = query.space_ids.clone().unwrap_or_default();
    if roots.is_empty() {
        return Ok((query, None));
    }
    let spaces = store.list_spaces()?;
    let id_to_name: HashMap<Uuid, String> = spaces.iter().map(|s| (s.id, s.name.clone())).collect();
    let links = store.list_space_links()?;
    let parents = bfs_reachable(&roots, &links, &id_to_name);
    query.space_ids = Some(parents.keys().copied().collect());
    Ok((
        query,
        Some(Traversal {
            roots,
            parents,
            id_to_name,
            spaces,
        }),
    ))
}

fn decorate(raw: SearchHitRaw, traversal: Option<&Traversal>, score: f64) -> SearchHit {
    let spaces = match traversal {
        None => raw
            .space_names
            .into_iter()
            .map(|name| SpaceMatch { name, via: None })
            .collect(),
        Some(traversal) => traversal.matches(raw.space_names),
    };
    SearchHit {
        memory_id: raw.memory_id,
        subject: raw.subject,
        content: raw.content,
        snippet: raw.snippet,
        score,
        state: raw.state,
        created_at: raw.created_at,
        extraction: raw.extraction,
        spaces,
        source_title: raw.source_title,
        source_location: raw.source_location,
    }
}

/// BFS over directed links from the root spaces. Returns `child -> parent`
/// for every reachable space (roots map to `None`). Neighbors are expanded
/// in lexicographic name order so traversal order is deterministic; cycles
/// terminate through the visited set.
fn bfs_reachable(
    roots: &[Uuid],
    links: &[SpaceLink],
    id_to_name: &HashMap<Uuid, String>,
) -> HashMap<Uuid, Option<Uuid>> {
    let mut adjacency: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for link in links {
        adjacency
            .entry(link.from_space)
            .or_default()
            .push(link.to_space);
    }
    let name_of = |id: &Uuid| id_to_name.get(id).cloned().unwrap_or_default();

    let mut visited: HashMap<Uuid, Option<Uuid>> = HashMap::new();
    let mut queue: VecDeque<Uuid> = VecDeque::new();
    let mut roots_sorted: Vec<Uuid> = roots.to_vec();
    roots_sorted.sort_by_key(name_of);
    for root in roots_sorted {
        match visited.entry(root) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(None);
                queue.push_back(root);
            }
            std::collections::hash_map::Entry::Occupied(_) => {}
        }
    }
    while let Some(current) = queue.pop_front() {
        let mut neighbors = adjacency.get(&current).cloned().unwrap_or_default();
        neighbors.sort_by_key(name_of);
        for neighbor in neighbors {
            match visited.entry(neighbor) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(Some(current));
                    queue.push_back(neighbor);
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
    }
    visited
}

/// Space names along the path from a root to `target`, root first; `None`
/// when the target is not reachable from any root.
fn path_from_root(
    target: Uuid,
    parents: &HashMap<Uuid, Option<Uuid>>,
    id_to_name: &HashMap<Uuid, String>,
) -> Option<Vec<String>> {
    if !parents.contains_key(&target) {
        return None;
    }
    let mut path = vec![target];
    let mut current = target;
    while let Some(Some(parent)) = parents.get(&current) {
        path.push(*parent);
        current = *parent;
    }
    path.reverse();
    Some(
        path.iter()
            .map(|id| id_to_name.get(id).cloned().unwrap_or_default())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Ts;
    use crate::storage::sqlite::SqliteStore;
    use std::cell::Cell;

    fn query(text: &str, limit: u32) -> SearchQuery {
        SearchQuery {
            text: text.to_string(),
            space_ids: None,
            limit,
            at: None,
            include_superseded: false,
            include_retired: false,
        }
    }

    fn link(from: Uuid, to: Uuid) -> SpaceLink {
        SpaceLink {
            from_space: from,
            to_space: to,
            created_at: Ts(0),
        }
    }

    fn name_map(entries: &[(Uuid, &str)]) -> HashMap<Uuid, String> {
        entries.iter().map(|(id, n)| (*id, n.to_string())).collect()
    }

    #[test]
    fn directed_links_do_not_imply_reverse_visibility() {
        let owlry = Uuid::now_v7();
        let coding = Uuid::now_v7();
        let linux = Uuid::now_v7();
        let names = name_map(&[
            (owlry, "project:owlry"),
            (coding, "coding"),
            (linux, "linux"),
        ]);
        let links = vec![link(owlry, coding), link(coding, linux)];

        let from_owlry = bfs_reachable(&[owlry], &links, &names);
        assert!(from_owlry.contains_key(&coding));
        assert!(from_owlry.contains_key(&linux));

        let from_linux = bfs_reachable(&[linux], &links, &names);
        assert!(from_linux.contains_key(&linux));
        assert!(
            !from_linux.contains_key(&coding),
            "reverse visibility leaked"
        );
        assert!(
            !from_linux.contains_key(&owlry),
            "reverse visibility leaked"
        );
    }

    #[test]
    fn cycles_terminate() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let names = name_map(&[(a, "a"), (b, "b")]);
        let links = vec![link(a, b), link(b, a)];
        let visited = bfs_reachable(&[a], &links, &names);
        assert_eq!(visited.len(), 2);
        assert_eq!(visited[&b], Some(a));
        assert_eq!(visited[&a], None);
    }

    #[test]
    fn neighbors_expand_in_lexicographic_order() {
        let root = Uuid::now_v7();
        let zeta = Uuid::now_v7();
        let alpha = Uuid::now_v7();
        let names = name_map(&[(root, "root"), (zeta, "zeta"), (alpha, "alpha")]);
        // Deliberately inserted in reverse lexicographic order.
        let links = vec![link(root, zeta), link(root, alpha)];
        let visited = bfs_reachable(&[root], &links, &names);
        assert_eq!(visited[&alpha], Some(root));
        assert_eq!(visited[&zeta], Some(root));
    }

    #[test]
    fn path_records_the_traversal() {
        let owlry = Uuid::now_v7();
        let coding = Uuid::now_v7();
        let linux = Uuid::now_v7();
        let names = name_map(&[
            (owlry, "project:owlry"),
            (coding, "coding"),
            (linux, "linux"),
        ]);
        let links = vec![link(owlry, coding), link(coding, linux)];
        let parents = bfs_reachable(&[owlry], &links, &names);

        let to_linux = path_from_root(linux, &parents, &names).unwrap();
        assert_eq!(to_linux, vec!["project:owlry", "coding", "linux"]);

        let to_coding = path_from_root(coding, &parents, &names).unwrap();
        assert_eq!(to_coding, vec!["project:owlry", "coding"]);

        let missing = Uuid::now_v7();
        assert!(path_from_root(missing, &parents, &names).is_none());
    }

    #[test]
    fn candidate_pool_is_bounded_and_never_below_the_limit() {
        assert_eq!(candidate_pool_size(0, None), 0);
        assert_eq!(candidate_pool_size(1, None), 20);
        assert_eq!(candidate_pool_size(10, None), 40);
        assert_eq!(candidate_pool_size(100, None), 200);
        assert_eq!(candidate_pool_size(500, None), 500);
        assert_eq!(candidate_pool_size(u32::MAX, None), u32::MAX);
        assert_eq!(candidate_pool_size(10, Some(5)), 10);
        assert_eq!(candidate_pool_size(10, Some(50)), 50);
    }

    #[test]
    fn rrf_constant_defaults_to_sixty() {
        assert_eq!(RRF_K_DEFAULT.get(), 60);
        assert_eq!(RetrievalOptions::default().rrf_k.get(), 60);
        assert_eq!(SearchMode::default(), SearchMode::Lexical);
    }

    #[test]
    fn mode_adapter_resolves_only_non_lexical_requests() {
        let store = SqliteStore::open_in_memory().unwrap();
        let resolutions = Cell::new(0);

        let lexical =
            search_with_mode_and_resolver(&store, query("missing", 0), SearchMode::Lexical, |_| {
                resolutions.set(resolutions.get() + 1);
                Ok(SemanticCapability::Unavailable(
                    FallbackReason::EmbeddingsDisabled,
                ))
            })
            .unwrap();
        assert_eq!(lexical.effective, EffectiveMode::NotRun);
        assert_eq!(resolutions.get(), 0);

        let modes = [SearchMode::Semantic, SearchMode::Hybrid, SearchMode::Auto];
        for (index, mode) in modes.into_iter().enumerate() {
            let result = search_with_mode_and_resolver(&store, query("missing", 0), mode, |_| {
                resolutions.set(resolutions.get() + 1);
                Ok(SemanticCapability::Unavailable(
                    FallbackReason::EmbeddingsDisabled,
                ))
            });
            assert_eq!(resolutions.get(), index + 1);
            match mode {
                SearchMode::Semantic | SearchMode::Hybrid => {
                    assert!(matches!(result, Err(Error::SemanticUnavailable { .. })));
                }
                SearchMode::Auto => {
                    let outcome = result.unwrap();
                    assert_eq!(outcome.effective, EffectiveMode::NotRun);
                    assert_eq!(outcome.fallback, None);
                }
                SearchMode::Lexical => unreachable!(),
            }
        }
    }

    #[test]
    fn mode_adapter_rejects_blank_queries_before_capability_resolution() {
        let store = SqliteStore::open_in_memory().unwrap();
        let resolutions = Cell::new(0);
        let result =
            search_with_mode_and_resolver(&store, query("   ", 0), SearchMode::Auto, |_| {
                resolutions.set(resolutions.get() + 1);
                Ok(SemanticCapability::Unavailable(
                    FallbackReason::EmbeddingsDisabled,
                ))
            });
        assert!(matches!(result, Err(Error::EmptyQuery)));
        assert_eq!(resolutions.get(), 0);
    }
}

/// Symbol search over the deterministic code-structure layer. Validates the
/// query, then delegates to the store. Results are definitions with their
/// location, not memories: they carry no score, confidence, or lifecycle.
pub fn search_code(
    store: &dyn Store,
    mut query: crate::core::CodeSearchQuery,
) -> Result<Vec<crate::core::CodeHit>> {
    query.text = query.text.trim().to_string();
    if query.text.is_empty() {
        return Err(Error::EmptyQuery);
    }
    if query.limit == 0 {
        return Ok(Vec::new());
    }
    query.limit = query.limit.min(100);
    store.search_code(&query)
}

/// Parses entity kind names for symbol search. Accepts the canonical names
/// (`function`, `type_alias`, ...) plus the usual short forms (`fn`, `type`,
/// `const`, `mod`, `use`).
pub fn parse_code_kinds<S: AsRef<str>>(names: &[S]) -> Result<Vec<crate::core::CodeEntityKind>> {
    names
        .iter()
        .map(|n| {
            let raw = n.as_ref().trim().to_lowercase();
            let canonical = match raw.as_str() {
                "fn" | "func" => "function",
                "type" => "type_alias",
                "const" => "constant",
                "mod" => "module",
                "use" => "import",
                other => other,
            };
            crate::core::CodeEntityKind::parse(canonical).ok_or_else(|| {
                Error::QuerySyntax(format!(
                    "unknown code kind '{}'; expected one of: module, struct, enum, trait, \
                     type_alias, function, method, constant, static, field, import",
                    n.as_ref()
                ))
            })
        })
        .collect()
}
