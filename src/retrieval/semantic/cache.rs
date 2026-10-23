//! One MCP session's verified runtime. Nothing here survives process exit.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{readiness, resolve_ready, ModelLookup, Readiness, ReadyModel, RuntimeFactory};
use super::{FallbackReason, SemanticCapability};
use crate::core::{Error, Result, SearchQuery};
use crate::retrieval::{search_with_mode, RetrievalOptions, SearchMode, SearchOutcome};
use crate::storage::Store;

#[derive(Default)]
pub(crate) struct SemanticCache {
    entry: Option<Cached>,
    last_observed: Option<SystemTime>,
}

struct Cached {
    model: ReadyModel,
    snapshot: Snapshot,
    capability: SemanticCapability,
}

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Identity {
    fn read(path: &Path) -> std::io::Result<Self> {
        let m = fs::symlink_metadata(path)?;
        Ok(Self {
            device: m.dev(),
            inode: m.ino(),
            mode: m.mode(),
            size: m.len(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        })
    }

    fn quiescent(&self, now: SystemTime) -> bool {
        let (Ok(secs), Ok(nanos)) = (u64::try_from(self.ctime.0), u32::try_from(self.ctime.1))
        else {
            return false;
        };
        if nanos >= 1_000_000_000 {
            return false;
        }
        UNIX_EPOCH
            .checked_add(Duration::new(secs, nanos))
            .and_then(|changed| now.duration_since(changed).ok())
            .is_some_and(|age| age >= Duration::from_secs(2))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    directories: Vec<Identity>,
    artifacts: Vec<Identity>,
}

impl Snapshot {
    fn read(base: &Path, model: &ReadyModel) -> std::io::Result<Self> {
        let directories = [base.to_path_buf(), base.join("models"), model.dir.clone()]
            .iter()
            .map(|path| Identity::read(path))
            .collect::<std::io::Result<_>>()?;
        let artifacts = model
            .entry
            .artifacts
            .iter()
            .map(|art| Identity::read(&model.dir.join(art.filename)))
            .collect::<std::io::Result<_>>()?;
        Ok(Self {
            directories,
            artifacts,
        })
    }

    fn quiescent(&self, now: SystemTime) -> bool {
        self.artifacts.iter().all(|id| id.quiescent(now))
    }
}

/// Only the local filesystem qualified for verification reuse so far. Unknown
/// filesystems keep the full-verification path, including on statfs failure.
fn qualified(model: &ReadyModel) -> bool {
    // linux/magic.h; rustix exposes statfs but not the btrfs magic constant.
    const BTRFS: rustix::fs::FsWord = 0x9123_683e_u32 as rustix::fs::FsWord;
    model.entry.artifacts.iter().all(|art| {
        rustix::fs::statfs(model.dir.join(art.filename)).is_ok_and(|stat| stat.f_type == BTRFS)
    })
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;

impl SemanticCache {
    pub(crate) fn search(
        &mut self,
        store: &dyn Store,
        query: SearchQuery,
        mode: SearchMode,
    ) -> Result<SearchOutcome> {
        self.search_with(
            store,
            query,
            mode,
            &super::manager::data_dir(),
            super::manifest::find_model,
            &super::candle_runtime,
            &SystemTime::now,
            &qualified,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn search_with(
        &mut self,
        store: &dyn Store,
        query: SearchQuery,
        mode: SearchMode,
        base: &Path,
        lookup: ModelLookup,
        factory: RuntimeFactory<'_>,
        clock: &dyn Fn() -> SystemTime,
        qualify: &dyn Fn(&ReadyModel) -> bool,
    ) -> Result<SearchOutcome> {
        if query.text.trim().is_empty() {
            return Err(Error::EmptyQuery);
        }
        if mode == SearchMode::Lexical {
            return search_with_mode(
                store,
                query,
                mode,
                &SemanticCapability::Unavailable(FallbackReason::EmbeddingsDisabled),
                RetrievalOptions::default(),
            );
        }

        // Taking ownership first makes every early error evict the old source.
        let previous = self.entry.take();
        let now = clock();
        let clock_forward = self.last_observed.is_none_or(|last| now >= last);
        self.last_observed = Some(now);
        let model = match readiness(store, base, lookup)? {
            Readiness::Ready(model) => model,
            Readiness::Unavailable(reason) => {
                return search_with_mode(
                    store,
                    query,
                    mode,
                    &SemanticCapability::Unavailable(reason),
                    RetrievalOptions::default(),
                );
            }
        };
        // Sample the clock before metadata and hashing. A newly written file
        // must be verified again after the timestamp-resolution safety margin.
        let before = Snapshot::read(base, &model).ok();
        let eligible = clock_forward
            && qualify(&model)
            && before.as_ref().is_some_and(|stamp| stamp.quiescent(now));
        let reused = eligible
            && previous
                .as_ref()
                .is_some_and(|old| old.model == model && Some(&old.snapshot) == before.as_ref());
        let capability = if reused {
            previous.expect("reused entry exists").capability
        } else {
            // Drop the old weights before allocating their replacement.
            drop(previous);
            resolve_ready(&model, factory)?
        };
        let result = search_with_mode(
            store,
            query.clone(),
            mode,
            &capability,
            RetrievalOptions::default(),
        )?;
        if !matches!(capability, SemanticCapability::Ready(_))
            || matches!(
                result.fallback,
                Some(FallbackReason::QueryEmbeddingFailed { .. })
            )
        {
            return Ok(result);
        }

        let after = Snapshot::read(base, &model).ok();
        if before.is_some() && before != after {
            // No semantic retry: Auto may run its ordinary lexical fallback.
            return search_with_mode(
                store,
                query,
                mode,
                &SemanticCapability::Unavailable(FallbackReason::ModelFilesInvalid {
                    model: model.entry.id.to_string(),
                    detail: "model artifacts changed during retrieval".to_string(),
                }),
                RetrievalOptions::default(),
            );
        }
        if eligible && qualify(&model) && (query.limit > 0 || reused) {
            self.entry = Some(Cached {
                model,
                snapshot: before.expect("eligible snapshot exists"),
                capability,
            });
        }
        Ok(result)
    }
}
