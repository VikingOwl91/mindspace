//! One MCP session's verified runtime. Nothing here survives process exit.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{readiness, resolve_ready, ModelLookup, Readiness, ReadyModel, RuntimeFactory};
use super::{FallbackReason, SemanticCapability};
use crate::core::{Error, Result, SearchQuery};
use crate::retrieval::{
    collect_prepared, concurrent_profile_change, prepare_plan, RetrievalOptions, SearchMode,
    SearchOutcome,
};
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
    pub(crate) fn search_named(
        &mut self,
        store: &dyn Store,
        query: SearchQuery,
        mode: SearchMode,
        names: Option<&[String]>,
    ) -> Result<SearchOutcome> {
        let base = match super::manager::data_dir() {
            Ok(base) => base,
            Err(e) => {
                self.entry = None;
                if query.text.trim().is_empty() {
                    return Err(Error::EmptyQuery);
                }
                let reason = if mode == SearchMode::Lexical {
                    FallbackReason::EmbeddingsDisabled
                } else {
                    FallbackReason::RuntimeUnavailable {
                        detail: e.to_string(),
                    }
                };
                let plan = prepare_plan(
                    mode,
                    &SemanticCapability::Unavailable(reason),
                    &query.text,
                    query.limit,
                )?;
                return collect_prepared(
                    store,
                    query,
                    mode,
                    plan,
                    RetrievalOptions::default(),
                    names,
                    None,
                )?
                .ok_or_else(concurrent_profile_change);
            }
        };
        self.search_with_named(
            store,
            query,
            mode,
            names,
            &base,
            super::manifest::find_model,
            &super::candle_runtime,
            &SystemTime::now,
            &qualified,
        )
    }

    #[cfg(test)]
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
        self.search_with_named(
            store, query, mode, None, base, lookup, factory, clock, qualify,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn search_with_named(
        &mut self,
        store: &dyn Store,
        query: SearchQuery,
        mode: SearchMode,
        names: Option<&[String]>,
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
            let plan = prepare_plan(
                mode,
                &SemanticCapability::Unavailable(FallbackReason::EmbeddingsDisabled),
                &query.text,
                query.limit,
            )?;
            return collect_prepared(
                store,
                query,
                mode,
                plan,
                RetrievalOptions::default(),
                names,
                None,
            )?
            .ok_or_else(concurrent_profile_change);
        }

        // Every early error evicts the old source. Only authenticated runtime
        // assets can be retained; settings and coverage are always read afresh.
        let mut previous = self.entry.take();
        let mut forced_asset_failure = None;
        for attempt in 0..=1 {
            let now = clock();
            let clock_forward = self.last_observed.is_none_or(|last| now >= last);
            self.last_observed = Some(now);
            let (profile, readiness) = readiness(store, base, lookup)?;
            let model = match readiness {
                Readiness::Ready(model) if forced_asset_failure.is_none() => Some(model),
                Readiness::Ready(_) => None,
                Readiness::Unavailable(reason) => {
                    let reason = forced_asset_failure.take().unwrap_or(reason);
                    let plan = prepare_plan(
                        mode,
                        &SemanticCapability::Unavailable(reason),
                        &query.text,
                        query.limit,
                    )?;
                    if let Some(result) = collect_prepared(
                        store,
                        query.clone(),
                        mode,
                        plan,
                        RetrievalOptions::default(),
                        names,
                        Some(&profile),
                    )? {
                        return Ok(result);
                    }
                    if attempt == 1 {
                        return Err(concurrent_profile_change());
                    }
                    previous = None;
                    continue;
                }
            };
            let Some(model) = model else {
                let reason = forced_asset_failure.take().expect("forced fallback exists");
                let plan = prepare_plan(
                    mode,
                    &SemanticCapability::Unavailable(reason),
                    &query.text,
                    query.limit,
                )?;
                return collect_prepared(
                    store,
                    query,
                    mode,
                    plan,
                    RetrievalOptions::default(),
                    names,
                    Some(&profile),
                )?
                .ok_or_else(concurrent_profile_change);
            };
            let before = Snapshot::read(base, &model).ok();
            let eligible = clock_forward
                && qualify(&model)
                && before.as_ref().is_some_and(|stamp| stamp.quiescent(now));
            let reused = eligible
                && previous.as_ref().is_some_and(|old| {
                    old.model == model && Some(&old.snapshot) == before.as_ref()
                });
            let capability = if reused {
                previous.take().expect("reused entry exists").capability
            } else {
                drop(previous.take());
                resolve_ready(&model, factory)?
            };
            // No read transaction is retained during inference.
            let plan = prepare_plan(mode, &capability, &query.text, query.limit)?;
            let Some(result) = collect_prepared(
                store,
                query.clone(),
                mode,
                plan,
                RetrievalOptions::default(),
                names,
                Some(&profile),
            )?
            else {
                if attempt == 1 {
                    return Err(concurrent_profile_change());
                }
                continue;
            };
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
                // Discard the whole response. A fresh lexical fallback (Auto)
                // consumes the same single restart budget as a DB mismatch;
                // strict modes retain their original explicit artifact error.
                let reason = FallbackReason::ModelFilesInvalid {
                    model: model.entry.id.to_string(),
                    detail: "model artifacts changed during retrieval".to_string(),
                };
                if mode != SearchMode::Auto {
                    return Err(crate::retrieval::unavailable(&reason));
                }
                if attempt == 1 {
                    return Err(concurrent_profile_change());
                }
                forced_asset_failure = Some(reason);
                continue;
            }
            if eligible && qualify(&model) && (query.limit > 0 || reused) {
                self.entry = Some(Cached {
                    model,
                    snapshot: before.expect("eligible snapshot exists"),
                    capability,
                });
            }
            return Ok(result);
        }
        Err(concurrent_profile_change())
    }
}
