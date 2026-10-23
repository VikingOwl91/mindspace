//! Reciprocal rank fusion over ranked assertion lists.
//!
//! Pure and storage-free: callers pass two ranked lists, best first. A list is
//! deduplicated by first occurrence before ranks are assigned, so an assertion
//! that appears more than once in one branch keeps its best position and counts once. Ranks
//! are 1-based.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;

use crate::core::Uuid;

/// One assertion after fusion. The ranks are its 1-based positions in each
/// assertion-deduplicated branch list; `None` means that branch did not return it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fused {
    pub assertion_id: Uuid,
    pub score: f64,
    pub lexical_rank: Option<u32>,
    pub semantic_rank: Option<u32>,
}

/// Keeps the first occurrence of each id and preserves order.
pub fn dedupe_in_order(ids: impl IntoIterator<Item = Uuid>) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    ids.into_iter().filter(|id| seen.insert(*id)).collect()
}

/// Fuses two ranked assertion lists. An assertion scores the sum of
/// `1 / (k + rank)` over the branches that return it.
///
/// Output is ordered by score descending, then by the best single-branch rank
/// ascending, then by assertion id ascending. Ids are unique, so this is a total
/// order: equal inputs always produce equal output. At most `limit` entries
/// are returned.
pub fn rrf_fuse(lexical: &[Uuid], semantic: &[Uuid], k: NonZeroU32, limit: usize) -> Vec<Fused> {
    let k = f64::from(k.get());
    let mut table: HashMap<Uuid, Fused> = HashMap::new();
    for (index, id) in dedupe_in_order(lexical.iter().copied())
        .into_iter()
        .enumerate()
    {
        let rank = rank_at(index);
        let entry = entry_for(&mut table, id);
        entry.lexical_rank = Some(rank);
        entry.score += 1.0 / (k + f64::from(rank));
    }
    for (index, id) in dedupe_in_order(semantic.iter().copied())
        .into_iter()
        .enumerate()
    {
        let rank = rank_at(index);
        let entry = entry_for(&mut table, id);
        entry.semantic_rank = Some(rank);
        entry.score += 1.0 / (k + f64::from(rank));
    }
    let mut fused: Vec<Fused> = table.into_values().collect();
    fused.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| best_rank(a).cmp(&best_rank(b)))
            .then_with(|| a.assertion_id.cmp(&b.assertion_id))
    });
    fused.truncate(limit);
    fused
}

/// 1-based rank for a zero-based position in a list.
pub(crate) fn rank_at(index: usize) -> u32 {
    u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX)
}

fn entry_for(table: &mut HashMap<Uuid, Fused>, id: Uuid) -> &mut Fused {
    table.entry(id).or_insert(Fused {
        assertion_id: id,
        score: 0.0,
        lexical_rank: None,
        semantic_rank: None,
    })
}

fn best_rank(fused: &Fused) -> u32 {
    match (fused.lexical_rank, fused.semantic_rank) {
        (Some(lexical), Some(semantic)) => lexical.min(semantic),
        (Some(rank), None) | (None, Some(rank)) => rank,
        (None, None) => u32::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn k(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    fn order(fused: &[Fused]) -> Vec<Uuid> {
        fused.iter().map(|entry| entry.assertion_id).collect()
    }

    #[test]
    fn known_inputs_match_hand_computed_scores() {
        // Lexical: A(1) B(2) C(3). Semantic: C(1) A(2) D(3). k = 60.
        let (a, b, c, d) = (id(1), id(2), id(3), id(4));
        let fused = rrf_fuse(&[a, b, c], &[c, a, d], k(60), 10);
        assert_eq!(order(&fused), vec![a, c, b, d]);
        assert!(close(fused[0].score, 1.0 / 61.0 + 1.0 / 62.0));
        assert!(close(fused[1].score, 1.0 / 63.0 + 1.0 / 61.0));
        assert!(close(fused[2].score, 1.0 / 62.0));
        assert!(close(fused[3].score, 1.0 / 63.0));
        assert_eq!(fused[0].lexical_rank, Some(1));
        assert_eq!(fused[0].semantic_rank, Some(2));
        assert_eq!(fused[2].semantic_rank, None);
        assert_eq!(fused[3].lexical_rank, None);
    }

    #[test]
    fn the_constant_is_applied_to_every_rank() {
        let small = rrf_fuse(&[id(1)], &[], k(1), 1);
        assert!(close(small[0].score, 0.5));
        let default = rrf_fuse(&[id(1)], &[], k(60), 1);
        assert!(close(default[0].score, 1.0 / 61.0));
    }

    #[test]
    fn duplicate_ids_keep_their_best_rank_and_count_once() {
        let (a, b) = (id(1), id(2));
        // A appears twice in the lexical list. After deduplication A is rank 1
        // and B is rank 2.
        let fused = rrf_fuse(&[a, a, b], &[b, a], k(60), 10);
        assert_eq!(fused.len(), 2);
        let fa = fused.iter().find(|entry| entry.assertion_id == a).unwrap();
        assert_eq!(fa.lexical_rank, Some(1));
        assert_eq!(fa.semantic_rank, Some(2));
        assert!(close(fa.score, 1.0 / 61.0 + 1.0 / 62.0));
        let fb = fused.iter().find(|entry| entry.assertion_id == b).unwrap();
        assert_eq!(fb.lexical_rank, Some(2));
        assert_eq!(fb.semantic_rank, Some(1));
        assert!(close(fb.score, 1.0 / 62.0 + 1.0 / 61.0));
        // The scores are equal and both best ranks are 1, so the id decides.
        assert_eq!(order(&fused), vec![a, b]);
    }

    #[test]
    fn exact_ties_break_by_best_rank_then_assertion_id() {
        let (lo, hi) = (id(1), id(2));
        let forward = rrf_fuse(&[lo, hi], &[hi, lo], k(60), 10);
        let reverse = rrf_fuse(&[hi, lo], &[lo, hi], k(60), 10);
        assert_eq!(forward[0].score, forward[1].score);
        assert_eq!(order(&forward), vec![lo, hi]);
        assert_eq!(order(&reverse), vec![lo, hi]);
    }

    #[test]
    fn one_branch_keeps_its_own_order() {
        let ids = [id(5), id(3), id(9)];
        let fused = rrf_fuse(&ids, &[], k(60), 10);
        assert_eq!(order(&fused), ids.to_vec());
        assert!(fused.iter().all(|entry| entry.semantic_rank.is_none()));
    }

    #[test]
    fn output_is_truncated_and_empty_inputs_are_empty() {
        let fused = rrf_fuse(&[id(1), id(2), id(3)], &[id(4)], k(60), 2);
        assert_eq!(fused.len(), 2);
        assert!(rrf_fuse(&[], &[], k(60), 10).is_empty());
        assert!(rrf_fuse(&[id(1)], &[], k(60), 0).is_empty());
    }
}
