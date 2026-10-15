//! Retrieval orchestration: executes vector / lexical / hybrid queries
//! against an open [`CollectionHandle`](crate::registry::CollectionHandle).
//!
//! Runs entirely on Tokio's blocking pool (callers use `web::block`):
//! tantivy search and HNSW search are CPU-bound. The lexical and store
//! mutexes are never held at the same time (matching the lock order used
//! by the ingestion runner), so queries cannot deadlock against writes.

use std::collections::BTreeMap;

use polyvec_core::{
    ChunkId, Error as CoreError, MetadataFilter, RetrievalMode, RetrievalPath, RrfConfig, ScoreKind,
};
use polyvec_retrieval::{LexicalHit, RankedHit, reciprocal_rank_fusion};
use polyvec_storage::CollectionStore;

use crate::MAX_K;
use crate::registry::{CollectionHandle, lock_lexical, lock_store};
use crate::schemas::{QueryResponseDto, RetrievalHitDto, metadata_to_json};

/// Over-fetch factor for filtered vector search. Metadata filters apply
/// post-retrieval on the vector path (the ANN index has no filter pushdown
/// in Phase 5), so more candidates than `k` are fetched and at most `k`
/// survive filtering. Documented in `docs/retrieval.md`.
const FILTER_OVERFETCH: usize = 4;

/// Parameters for one retrieval query (validated by the route beforehand).
pub struct QueryParams<'a> {
    /// Query text: embedded for the vector side, parsed for the lexical side.
    pub text: &'a str,
    /// Embedded query vector. `None` only in `lexical` mode (which needs
    /// no embedding provider).
    pub query_vector: Option<&'a [f32]>,
    /// Which subsystem(s) answer.
    pub mode: RetrievalMode,
    /// Number of hits.
    pub k: usize,
    /// HNSW search width; 0 selects the collection default.
    pub ef: usize,
    /// Exact-match metadata filter (all modes).
    pub filter: Option<&'a MetadataFilter>,
    /// RRF tuning for `hybrid` mode.
    pub rrf: &'a RrfConfig,
}

/// A ranked candidate before enrichment.
struct Candidate {
    /// Primary id: vector id in `vector` mode, chunk id otherwise.
    id: String,
    score: f64,
    score_kind: ScoreKind,
    path: Vec<RetrievalPath>,
    chunk_id: Option<ChunkId>,
    vector_id: Option<String>,
    distance: Option<f32>,
}

/// Executes one retrieval query and enriches hits from the catalog.
///
/// Failure behavior (documented in `docs/retrieval.md`): a failing
/// subsystem fails the whole query — there is no silent fallback. A
/// subsystem with no matches simply contributes an empty list.
///
/// # Errors
/// [`CoreError::Storage`] on index failures; [`CoreError::Validation`] on
/// a syntactically invalid lexical query; [`CoreError::Internal`] when a
/// vector-side query arrives without a query vector (a route bug, never
/// user error).
pub fn run_query(
    handle: &CollectionHandle,
    params: &QueryParams<'_>,
) -> Result<QueryResponseDto, CoreError> {
    // Lexical side first, under its own lock (released before the store
    // lock below — the two are never held together).
    let lex_hits: Vec<LexicalHit> = match params.mode {
        RetrievalMode::Vector => Vec::new(),
        _ => lock_lexical(&handle.lexical)?.search(params.text, params.k, params.filter)?,
    };

    let store = lock_store(&handle.store)?;
    let candidates = match params.mode {
        RetrievalMode::Vector => vector_candidates(&store, params)?,
        RetrievalMode::Lexical => lexical_candidates(&lex_hits),
        RetrievalMode::Hybrid => hybrid_candidates(&store, params, &lex_hits)?,
    };
    let effective_ef = match params.mode {
        RetrievalMode::Lexical => None,
        _ => Some(resolve_ef(&store, params.ef)?),
    };
    let mut hits = Vec::with_capacity(candidates.len());
    for cand in &candidates {
        let enriched = match &cand.chunk_id {
            Some(chunk_id) => enrich(&store, chunk_id)?,
            None => None,
        };
        // A candidate whose chunk/document vanished mid-query is skipped,
        // never served half-built.
        if cand.chunk_id.is_some() && enriched.is_none() {
            log::warn!("query: hit '{}' lost its chunk; skipping", cand.id);
            continue;
        }
        let e = enriched.unwrap_or_default();
        hits.push(RetrievalHitDto {
            rank: 0, // assigned below, after stale hits are dropped
            id: cand.id.clone(),
            score: cand.score,
            score_kind: cand.score_kind,
            path: cand.path.clone(),
            chunk_id: cand.chunk_id.as_ref().map(|c| c.as_str().to_string()),
            document_id: e.document_id,
            vector_id: cand.vector_id.clone(),
            distance: cand.distance,
            source: e.source,
            text: e.text,
            metadata: e.metadata,
        });
    }
    for (pos, hit) in hits.iter_mut().enumerate() {
        hit.rank = pos + 1;
    }
    Ok(QueryResponseDto {
        mode: params.mode,
        hits,
        k: params.k,
        ef: effective_ef,
        rrf_k: match params.mode {
            RetrievalMode::Hybrid => Some(params.rrf.k),
            _ => None,
        },
    })
}

/// Chunk text + provenance for one hit, joined from the catalog.
#[derive(Default)]
struct Enriched {
    document_id: Option<String>,
    source: Option<String>,
    text: Option<String>,
    metadata: Option<BTreeMap<String, serde_json::Value>>,
}

fn enrich(store: &CollectionStore, chunk_id: &ChunkId) -> Result<Option<Enriched>, CoreError> {
    let Some(chunk) = store.get_chunk(chunk_id)? else {
        return Ok(None);
    };
    let document_id = chunk.document_id.clone();
    let doc = match store.get_document(&document_id) {
        Ok((doc, _)) => doc,
        Err(CoreError::NotFound { .. }) => return Ok(None),
        Err(other) => return Err(other),
    };
    Ok(Some(Enriched {
        document_id: Some(document_id.as_str().to_string()),
        source: Some(doc.source.clone()),
        text: Some(chunk.text.clone()),
        metadata: Some(metadata_to_json(&doc.metadata)),
    }))
}

/// Whether a chunk's document satisfies the metadata filter. A chunk with
/// no document never matches (stale references are filtered out, not
/// served).
fn chunk_matches(
    store: &CollectionStore,
    chunk_id: &ChunkId,
    filter: &MetadataFilter,
) -> Result<bool, CoreError> {
    let Some(chunk) = store.get_chunk(chunk_id)? else {
        return Ok(false);
    };
    let doc = match store.get_document(&chunk.document_id) {
        Ok((doc, _)) => doc,
        Err(CoreError::NotFound { .. }) => return Ok(false),
        Err(other) => return Err(other),
    };
    Ok(filter.matches(&doc.metadata))
}

fn resolve_ef(store: &CollectionStore, ef: usize) -> Result<usize, CoreError> {
    let resolved = if ef == 0 {
        store.config().hnsw.ef_search
    } else {
        ef
    };
    if !(crate::MIN_SEARCH_EF..=crate::MAX_SEARCH_EF).contains(&resolved) {
        return Err(CoreError::Validation {
            detail: format!(
                "ef_search {resolved} is outside {}..={}",
                crate::MIN_SEARCH_EF,
                crate::MAX_SEARCH_EF
            ),
        });
    }
    Ok(resolved)
}

/// Vector-mode candidates: ANN search, post-filtered, truncated to `k`.
fn vector_candidates(
    store: &CollectionStore,
    params: &QueryParams<'_>,
) -> Result<Vec<Candidate>, CoreError> {
    let query_vector = params.query_vector.ok_or_else(|| CoreError::Internal {
        detail: "vector query without a query vector".to_string(),
    })?;
    let ef = resolve_ef(store, params.ef)?;
    let fetch_k = if params.filter.is_some() {
        (params.k * FILTER_OVERFETCH).min(MAX_K).max(params.k)
    } else {
        params.k
    };
    let mut candidates = Vec::new();
    for hit in store.search(query_vector, fetch_k, ef)? {
        if let Some(filter) = params.filter {
            match hit.vector_id.chunk_id() {
                Some(chunk_id) => {
                    if !chunk_matches(store, &chunk_id, filter)? {
                        continue;
                    }
                }
                // Raw vectors carry no document metadata: they cannot
                // satisfy a filter, so they are excluded (documented).
                None => continue,
            }
        }
        candidates.push(Candidate {
            id: hit.vector_id.as_str().to_string(),
            score: f64::from(hit.distance),
            score_kind: ScoreKind::Distance,
            path: vec![RetrievalPath::Vector],
            chunk_id: hit.vector_id.chunk_id(),
            vector_id: Some(hit.vector_id.as_str().to_string()),
            distance: Some(hit.distance),
        });
        if candidates.len() >= params.k {
            break;
        }
    }
    Ok(candidates)
}

/// Lexical-mode candidates: BM25 order straight from tantivy (filters are
/// pushed into the tantivy query itself).
fn lexical_candidates(lex_hits: &[LexicalHit]) -> Vec<Candidate> {
    lex_hits
        .iter()
        .map(|hit| Candidate {
            id: hit.chunk_id.as_str().to_string(),
            score: f64::from(hit.score),
            score_kind: ScoreKind::Bm25,
            path: vec![RetrievalPath::Lexical],
            chunk_id: Some(hit.chunk_id.clone()),
            vector_id: None,
            distance: None,
        })
        .collect()
}

/// Hybrid-mode candidates: RRF over the filtered vector and lexical
/// rankings. Vector hits without a chunk cannot fuse (nothing to join
/// text/provenance to) and are excluded — documented in
/// `docs/retrieval.md`.
fn hybrid_candidates(
    store: &CollectionStore,
    params: &QueryParams<'_>,
    lex_hits: &[LexicalHit],
) -> Result<Vec<Candidate>, CoreError> {
    let query_vector = params.query_vector.ok_or_else(|| CoreError::Internal {
        detail: "hybrid query without a query vector".to_string(),
    })?;
    let ef = resolve_ef(store, params.ef)?;
    let fetch_k = if params.filter.is_some() {
        (params.k * FILTER_OVERFETCH).min(MAX_K).max(params.k)
    } else {
        params.k
    };
    // Vector side, filtered to chunk-bearing hits (fusion key = chunk id).
    let mut vec_list: Vec<RankedHit> = Vec::new();
    let mut vec_info: BTreeMap<String, (String, f32)> = BTreeMap::new();
    for hit in store.search(query_vector, fetch_k, ef)? {
        let Some(chunk_id) = hit.vector_id.chunk_id() else {
            continue;
        };
        if let Some(filter) = params.filter
            && !chunk_matches(store, &chunk_id, filter)?
        {
            continue;
        }
        vec_info.insert(
            chunk_id.as_str().to_string(),
            (hit.vector_id.as_str().to_string(), hit.distance),
        );
        vec_list.push(RankedHit {
            id: chunk_id,
            rank: 0, // position in the vec is the rank; fixed below
        });
        if vec_list.len() >= params.k {
            break;
        }
    }
    for (pos, hit) in vec_list.iter_mut().enumerate() {
        hit.rank = u32::try_from(pos).unwrap_or(u32::MAX).saturating_add(1);
    }
    let lex_list: Vec<RankedHit> = lex_hits
        .iter()
        .enumerate()
        .map(|(pos, hit)| RankedHit {
            id: hit.chunk_id.clone(),
            rank: u32::try_from(pos).unwrap_or(u32::MAX).saturating_add(1),
        })
        .collect();

    let fused = reciprocal_rank_fusion(&[vec_list, lex_list], params.rrf);
    let mut candidates = Vec::new();
    for f in fused.into_iter().take(params.k) {
        let key = f.id.as_str().to_string();
        let (vector_id, distance) = vec_info.get(&key).cloned().unzip();
        let mut path = Vec::with_capacity(f.sources.len());
        for source in &f.sources {
            path.push(match source {
                0 => RetrievalPath::Vector,
                _ => RetrievalPath::Lexical,
            });
        }
        candidates.push(Candidate {
            id: key,
            score: f.score,
            score_kind: ScoreKind::Rrf,
            path,
            chunk_id: Some(f.id),
            vector_id,
            distance,
        });
    }
    Ok(candidates)
}
