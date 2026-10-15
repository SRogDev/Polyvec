//! Retrieval orchestration: executes vector / lexical / hybrid queries
//! against an open [`CollectionHandle`](crate::registry::CollectionHandle).
//!
//! Runs entirely on Tokio's blocking pool (callers use `web::block`):
//! tantivy search and HNSW search are CPU-bound. The lexical and store
//! mutexes are never held at the same time (matching the lock order used
//! by the ingestion runner), so queries cannot deadlock against writes.

use std::collections::{BTreeMap, HashSet};

use polyvec_core::{
    ChunkId, DocumentId, Error as CoreError, ExpandConfig, MetadataFilter, RetrievalMode,
    RetrievalPath, RrfConfig, ScoreKind,
};
use polyvec_retrieval::{
    Direction, Edge, LexicalHit, RankedHit, TraversalLimits, reciprocal_rank_fusion, traverse,
};
use polyvec_storage::CollectionStore;

use crate::MAX_K;
use crate::registry::{CollectionHandle, lock_lexical, lock_store};
use crate::schemas::{
    ExpandEchoDto, QueryResponseDto, RelationEvidenceDto, RelationHopDto, RetrievalHitDto,
    metadata_to_json,
};

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
    /// Relationship expansion (Phase 6). `None` = Phase 5 behavior.
    pub expand: Option<&'a ExpandConfig>,
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
            evidence: None,
        });
    }
    for (pos, hit) in hits.iter_mut().enumerate() {
        hit.rank = pos + 1;
    }
    // Relationship expansion (Phase 6): bounded BFS seeded from the base
    // hits, appended after them with path evidence.
    if let Some(expand) = params.expand {
        expand_hits(&store, &mut hits, expand)?;
        for (pos, hit) in hits.iter_mut().enumerate() {
            hit.rank = pos + 1;
        }
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
        expand: params.expand.map(ExpandEchoDto::from),
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
// ---------------------------------------------------------------------------
// relationship expansion (Phase 6)
// ---------------------------------------------------------------------------

/// Default fan-out cap for query-time traversal: supernode protection
/// (a document cited by thousands of others). See `docs/retrieval.md`.
const EXPAND_FANOUT: usize = 64;
/// Wall-clock backstop for query-time traversal, in milliseconds.
const EXPAND_MAX_MILLIS: u64 = 2000;

/// Enrichment fields for one expanded node.
struct ExpandedEnriched {
    chunk_id: Option<String>,
    document_id: Option<String>,
    source: Option<String>,
    text: Option<String>,
    metadata: Option<BTreeMap<String, serde_json::Value>>,
}

/// Enriches an expanded node: chunk nodes get the chunk treatment,
/// document nodes get document-level fields (a document has no chunk
/// text of its own). Returns `None` when the endpoint vanished
/// mid-query — the caller skips it, never serving half-built hits.
fn enrich_expanded(
    store: &CollectionStore,
    node: &str,
) -> Result<Option<ExpandedEnriched>, CoreError> {
    // Chunk and document ids share a loose validated shape; disambiguate
    // by table lookup, chunk first (a string is never both).
    if let Ok(chunk_id) = ChunkId::parse(node) {
        if let Some(e) = enrich(store, &chunk_id)? {
            return Ok(Some(ExpandedEnriched {
                chunk_id: Some(chunk_id.as_str().to_string()),
                document_id: e.document_id,
                source: e.source,
                text: e.text,
                metadata: e.metadata,
            }));
        }
    }
    if let Ok(doc_id) = DocumentId::parse(node) {
        match store.get_document(&doc_id) {
            Ok((doc, _)) => {
                return Ok(Some(ExpandedEnriched {
                    chunk_id: None,
                    document_id: Some(doc_id.as_str().to_string()),
                    source: Some(doc.source.clone()),
                    text: None,
                    metadata: Some(metadata_to_json(&doc.metadata)),
                }));
            }
            Err(CoreError::NotFound { .. }) => {}
            Err(other) => return Err(other),
        }
    }
    Ok(None)
}

/// Runs bounded relationship expansion and appends the reached nodes as
/// hits after the base hits (ranks are reassigned by the caller).
///
/// Seeds are the chunk + document ids of every base hit. Raw-vector hits
/// (no chunk/document) contribute no seeds. Nodes already present as base
/// hits are not re-added.
fn expand_hits(
    store: &CollectionStore,
    hits: &mut Vec<RetrievalHitDto>,
    expand: &ExpandConfig,
) -> Result<(), CoreError> {
    let mut seeds: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let base_ids: HashSet<String> = hits.iter().map(|h| h.id.clone()).collect();
    for hit in hits.iter() {
        for id in [&hit.chunk_id, &hit.document_id].into_iter().flatten() {
            if seen.insert(id.clone()) {
                seeds.push(id.clone());
            }
        }
    }
    if seeds.is_empty() {
        return Ok(());
    }
    let limits = TraversalLimits {
        max_depth: expand.depth,
        max_nodes: expand.max_nodes,
        max_fanout: EXPAND_FANOUT,
        max_millis: EXPAND_MAX_MILLIS,
    };
    let expanded = traverse(
        &seeds,
        |node| -> Result<Vec<Edge>, CoreError> {
            let mut edges = Vec::new();
            for rel in store.relations_incident(node)? {
                if !expand.allows(rel.relation_type) {
                    continue;
                }
                let outgoing = rel.source == node;
                edges.push(Edge {
                    relation_id: rel.id.as_str().to_string(),
                    relation_type: rel.relation_type,
                    direction: if outgoing {
                        Direction::Outgoing
                    } else {
                        Direction::Incoming
                    },
                    neighbor: if outgoing {
                        rel.target.clone()
                    } else {
                        rel.source.clone()
                    },
                });
            }
            Ok(edges)
        },
        &limits,
    )?;
    for node in &expanded {
        if base_ids.contains(&node.node) {
            continue;
        }
        let Some(enriched) = enrich_expanded(store, &node.node)? else {
            log::warn!("expand: node '{}' lost its endpoint; skipping", node.node);
            continue;
        };
        hits.push(RetrievalHitDto {
            rank: 0, // reassigned by the caller
            id: node.node.clone(),
            // Hops from the nearest base hit; lower is better. Never
            // compared with base-hit scores (different universe — see
            // docs/retrieval.md).
            score: f64::from(node.hops),
            score_kind: ScoreKind::Relationship,
            path: vec![RetrievalPath::Relationship],
            chunk_id: enriched.chunk_id,
            document_id: enriched.document_id,
            // Expanded hits have no vector origin: no vector id, no
            // distance.
            vector_id: None,
            distance: None,
            source: enriched.source,
            text: enriched.text,
            metadata: enriched.metadata,
            evidence: Some(RelationEvidenceDto {
                hops: node.hops,
                path: node
                    .path
                    .iter()
                    .map(|hop| RelationHopDto {
                        relation_id: hop.relation_id.clone(),
                        relation_type: hop.relation_type,
                        direction: match hop.direction {
                            Direction::Outgoing => "outgoing",
                            Direction::Incoming => "incoming",
                        },
                    })
                    .collect(),
            }),
        });
    }
    Ok(())
}
