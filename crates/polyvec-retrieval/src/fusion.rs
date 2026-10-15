//! Reciprocal Rank Fusion (RRF) over per-subsystem rankings.
//!
//! RRF fuses ranked lists without needing comparable scores: each
//! subsystem contributes `1 / (k + rank)` per chunk (ranks 1-based), and
//! the fused score is the sum. A chunk that ranks highly in *several*
//! subsystems beats one that ranks highly in only one — the standard
//! behavior that makes hybrid (vector + lexical) retrieval robust.
//!
//! The function is pure and deterministic: ties break by chunk id, so the
//! same input lists always produce the same output ordering.

use std::collections::BTreeMap;

use polyvec_core::{ChunkId, RrfConfig};

/// One ranked entry from a subsystem's result list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedHit {
    /// The chunk this hit refers to.
    pub id: ChunkId,
    /// 1-based rank within its subsystem list (1 = best).
    pub rank: u32,
}

/// A fused hit: RRF score, final rank, and provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct FusedHit {
    /// The chunk.
    pub id: ChunkId,
    /// Σ over input lists of `1 / (k + rank)`. Higher is better. Only
    /// comparable within a single fusion call — never across queries.
    pub score: f64,
    /// 1-based rank in the fused ordering.
    pub rank: u32,
    /// Indices of the input lists that contained this chunk, ascending.
    pub sources: Vec<usize>,
}

/// Fuses per-subsystem rankings with Reciprocal Rank Fusion.
///
/// `lists[i]` is the i-th subsystem's ranking (best first; `rank` fields
/// are ignored — position in the vector is the rank). A chunk appearing in
/// several lists accumulates contributions from each. Duplicate chunks
/// *within* one list keep their best (lowest) rank.
///
/// The output is sorted by descending score with ties broken by chunk id,
/// then assigned 1-based ranks.
#[must_use]
pub fn reciprocal_rank_fusion(lists: &[Vec<RankedHit>], config: &RrfConfig) -> Vec<FusedHit> {
    // Accumulate per-chunk: (score, sources). BTreeMap keeps the
    // accumulation order deterministic even before sorting.
    let mut acc: BTreeMap<String, (f64, Vec<usize>, ChunkId)> = BTreeMap::new();
    for (list_idx, list) in lists.iter().enumerate() {
        // Defensive: a subsystem must never emit the same chunk twice, but
        // if it does, the best rank wins rather than double-counting.
        let mut seen_in_list = std::collections::HashSet::new();
        for (pos, hit) in list.iter().enumerate() {
            let key = hit.id.as_str().to_string();
            if !seen_in_list.insert(key.clone()) {
                continue;
            }
            let rank = pos as u64 + 1;
            #[allow(clippy::cast_precision_loss)]
            let contribution = 1.0 / (config.k + rank as f64);
            let entry = acc
                .entry(key)
                .or_insert_with(|| (0.0, Vec::new(), hit.id.clone()));
            entry.0 += contribution;
            if !entry.1.contains(&list_idx) {
                entry.1.push(list_idx);
            }
        }
    }
    let mut fused: Vec<FusedHit> = acc
        .into_values()
        .map(|(score, mut sources, id)| {
            sources.sort_unstable();
            FusedHit {
                id,
                score,
                rank: 0, // assigned after sorting
                sources,
            }
        })
        .collect();
    // Sort by descending score; f64 total order via total_cmp (no NaN can
    // arise: k >= 1 and rank >= 1 keep every term finite and positive).
    // Ties break by chunk id for determinism.
    fused.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    for (pos, hit) in fused.iter_mut().enumerate() {
        hit.rank = u32::try_from(pos).unwrap_or(u32::MAX).saturating_add(1);
    }
    fused
}

#[cfg(test)]
mod tests {
    use super::*;
    use polyvec_core::{CollectionId, DocumentId};

    fn chunk(collection: &str, doc: &str, index: u32) -> ChunkId {
        let coll = CollectionId::new(collection).unwrap();
        let doc_id = DocumentId::for_source(&coll, doc);
        ChunkId::for_index(&doc_id, index)
    }

    fn ranked(ids: &[ChunkId]) -> Vec<RankedHit> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| RankedHit {
                id: id.clone(),
                rank: u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1),
            })
            .collect()
    }

    #[test]
    fn fuses_two_lists_with_hand_computed_scores() {
        let x = chunk("c", "doc-x", 0);
        let y = chunk("c", "doc-y", 0);
        let z = chunk("c", "doc-z", 0);
        // List A (vector): x > y.  List B (lexical): y > z.
        let lists = vec![
            ranked(&[x.clone(), y.clone()]),
            ranked(&[y.clone(), z.clone()]),
        ];
        let fused = reciprocal_rank_fusion(&lists, &RrfConfig::default());
        // k=60: y = 1/62 + 1/61 ≈ 0.03252, x = 1/61 ≈ 0.01639, z = 1/62 ≈ 0.01613.
        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].id, y);
        assert_eq!(fused[0].rank, 1);
        assert_eq!(fused[0].sources, vec![0, 1]);
        assert!((fused[0].score - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-12);
        assert_eq!(fused[1].id, x);
        assert_eq!(fused[1].sources, vec![0]);
        assert!((fused[1].score - 1.0 / 61.0).abs() < 1e-12);
        assert_eq!(fused[2].id, z);
        assert_eq!(fused[2].sources, vec![1]);
        assert!((fused[2].score - 1.0 / 62.0).abs() < 1e-12);
    }

    #[test]
    fn empty_input_gives_empty_output() {
        assert!(reciprocal_rank_fusion(&[], &RrfConfig::default()).is_empty());
        assert!(reciprocal_rank_fusion(&[vec![], vec![]], &RrfConfig::default()).is_empty());
    }

    #[test]
    fn single_list_preserves_order() {
        let a = chunk("c", "a", 0);
        let b = chunk("c", "b", 0);
        let c = chunk("c", "c", 0);
        let fused = reciprocal_rank_fusion(
            &[ranked(&[a.clone(), b.clone(), c.clone()])],
            &RrfConfig::default(),
        );
        let ids: Vec<_> = fused.iter().map(|h| h.id.clone()).collect();
        assert_eq!(ids, vec![a, b, c]);
        assert!(fused[0].score > fused[1].score && fused[1].score > fused[2].score);
    }

    #[test]
    fn ties_break_deterministically_by_chunk_id() {
        // Two chunks each appearing once at rank 1 in different lists:
        // identical scores — the lower chunk id must win, every time.
        let a = chunk("c", "aaa", 0);
        let b = chunk("c", "zzz", 0);
        let lists = vec![
            ranked(std::slice::from_ref(&a)),
            ranked(std::slice::from_ref(&b)),
        ];
        let config = RrfConfig::default();
        let first = reciprocal_rank_fusion(&lists, &config);
        let swapped = reciprocal_rank_fusion(&[lists[1].clone(), lists[0].clone()], &config);
        assert_eq!(first[0].id, a);
        assert_eq!(swapped[0].id, a, "tie-break must not depend on list order");
        // Identical inputs produce bit-identical scores; exact comparison is intended.
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(first[0].score, first[1].score);
        }
    }

    #[test]
    fn duplicate_within_one_list_keeps_best_rank() {
        let a = chunk("c", "a", 0);
        let b = chunk("c", "b", 0);
        // a appears at rank 1 AND rank 3 of the same list (subsystem bug):
        // it must count once, at rank 1.
        let list = vec![
            RankedHit {
                id: a.clone(),
                rank: 1,
            },
            RankedHit {
                id: b.clone(),
                rank: 2,
            },
            RankedHit {
                id: a.clone(),
                rank: 3,
            },
        ];
        let fused = reciprocal_rank_fusion(&[list], &RrfConfig::default());
        assert_eq!(fused.len(), 2);
        assert!((fused[0].score - 1.0 / 61.0).abs() < 1e-12);
    }

    #[test]
    fn smaller_k_rewards_top_ranks_more() {
        // x ranks 1st in list A only; y ranks 2nd in list A and 1st in list B.
        // A narrow k concentrates mass on rank 1, so x's single rank-1
        // contribution is worth relatively more than under the wide default.
        let x = chunk("c", "x", 0);
        let y = chunk("c", "y", 0);
        let lists = vec![
            ranked(&[x.clone(), y.clone()]),
            ranked(std::slice::from_ref(&y)),
        ];
        let wide = reciprocal_rank_fusion(&lists, &RrfConfig::new(60.0).unwrap());
        let narrow = reciprocal_rank_fusion(&lists, &RrfConfig::new(1.0).unwrap());
        let ratio = |fused: &[FusedHit]| {
            let sx = fused.iter().find(|h| h.id == x).unwrap().score;
            let sy = fused.iter().find(|h| h.id == y).unwrap().score;
            sx / sy
        };
        // Narrow k concentrates mass on rank 1, so x's single rank-1
        // contribution is worth relatively more.
        assert!(
            ratio(&narrow) > ratio(&wide),
            "narrow k should favor the rank-1 hit relatively more"
        );
    }

    #[test]
    fn three_lists_accumulate() {
        let a = chunk("c", "a", 0);
        let lists = vec![
            ranked(std::slice::from_ref(&a)),
            ranked(std::slice::from_ref(&a)),
            ranked(std::slice::from_ref(&a)),
        ];
        let fused = reciprocal_rank_fusion(&lists, &RrfConfig::default());
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].sources, vec![0, 1, 2]);
        assert!((fused[0].score - 3.0 / 61.0).abs() < 1e-12);
    }
}
