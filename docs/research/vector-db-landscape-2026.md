# Polyvec — Vector DB / Knowledge Base Landscape (2026)

Research date: 2026-09-26. Purpose: ground the build plan for **Polyvec**, a from-scratch Rust vector DB designed as a *company knowledge base* where agents issue massive numbers of cheap read queries. Current reference: Polygrow uses Supermemory; Pinecone was considered and rejected for the knowledge+memory split.

> All benchmark figures below are from third-party 2025–2026 sources (linked). Treat them as directional, not gospel — hardware and datasets vary.

---

## 1. Existing Rust pieces: reuse vs build

### 1.1 The pieces that exist

| Piece | Status (2026) | Verdict |
|---|---|---|
| **Qdrant** (Rust, Apache-2.0) | The reference open-source vector DB. Custom HNSW with metadata filtering *inside* the traversal loop (single-stage filtered search), SQ/PQ/BQ quantization, mmap on-disk vectors, payload indexes, distributed mode, p99 ~3.8 ms self-hosted | Study it deeply; **do not re-implement a worse Qdrant**. Differentiate above it (see §6) |
| **tantivy** (Rust, MIT) | Mature full-text engine (BM25). The obvious BM25 sidecar for hybrid search | **Reuse as-is** |
| **hnsw_rs** (`jean-pierreBoth/hnswlib-rs`, pure Rust, MIT/Apache) | Maintained pure-Rust HNSW with incremental insert/search, customizable distances. Used as the ANN core by at least one 2026 knowledge-DB project ("brain-db") as a library, layering concurrency + filtering on top | **Reuse as library for v0.1**; fork only if filter hooks or quantization integration demand it |
| **usearch** (C++ core, Rust bindings) | Best-in-class embedded HNSW; incremental add *and* remove, f16/i8 quantization, `mmap` view for instant cold start, clean static link. Won a 20-candidate 2026 audit for an offline single-binary product | **Strong alternative** if pure-Rust is not a hard constraint |
| **instant-distance** | Frozen since 2023; rebuild-only, no incremental insert/delete | **Do not use** |
| **hora** | Abandoned; known NaN panics on cosine since 2021 | **Do not use** |
| **arroy** | Deprecated by Meilisearch (moved to `hannoy`); RP-trees weak at 768-dim | **Do not use** |
| **redb** (pure Rust embedded KV) | Solid transactional KV store; good fit for metadata/payload + WAL | **Reuse** for the metadata/payload layer |
| **roaring-rs** | Roaring bitmaps — the standard substrate for metadata filter bitmaps | **Reuse** |
| **ort** (ONNX Runtime bindings) / **candle** | For in-process embedding inference (see §5) | **Reuse** (`ort` is more mature; `candle` is pure-Rust but younger) |
| **sqlite-vec** | Stable release still brute-force ANN — fine for tiny corpora, not a core | Ignore for the core |

### 1.2 Qdrant is already Rust — what would a new DB do differently?

Qdrant is a *general-purpose* vector database. It is excellent at what it is. A new DB earns its existence by being **workload-shaped**, not by beating Qdrant's HNSW micro-benchmarks (you won't, and it doesn't matter):

1. **Read-cost-shaped architecture.** Qdrant Cloud bills vCPU+RAM hourly; Pinecone bills per read unit. Polyvec's thesis: a single self-hosted binary where marginal query cost ≈ 0 — quantized-by-default indexes, embedding cache, batched query API. The moat is the *cost model*, not the index.
2. **Knowledge-base-native semantics.** Qdrant stores points + payload. A company KB needs: temporal truth (supersession — "we moved to SF" invalidates "we live in NYC"), entity identity across documents, ACLs enforced at retrieval time, contextual chunking built into ingestion. This is the layer Supermemory sells as a service; Polyvec builds it self-hosted and open.
3. **Agent-native API.** Weaviate shipped a native MCP server in April 2026 — the direction is clear. Verbs like `remember / search / forget / supersede` over a raw `upsert/query` API.

---

## 2. ANN algorithms: do we need new retrieval math?

**Direct answer: no.** The retrieval math you need already exists, is open, and is more than good enough. The moat is composition and engineering.

### 2.1 The 2026 algorithm map

| Family | Representative | Recall@10 | Memory/vec (768-dim) | Notes |
|---|---|---|---|---|
| Graph (in-RAM) | **HNSW** (M=16) | 0.95–0.99 | ~3.4 KB | Default for <10M vectors; best single-query latency |
| Graph (disk) | **DiskANN / Vamana** | 0.95–0.98 | ~200 B RAM + SSD | Billion-scale on one box; higher latency |
| Partition + PQ | **IVF-PQ** | 0.80–0.92 | 32–128 B | RAM-tight large scale; needs training |
| Learned quant | **ScaNN** (anisotropic) | 0.90–0.96 | 16–64 B | Google's; less Rust-friendly |
| Hybrid | SPANN | 0.90–0.95 | ~200 B + SSD | Billion-scale |

(Consolidated from the performance-engineering handbook's ANN chapter and independent 2026 benchmarks.)

### 2.2 The quantization frontier that actually matters

The real 2024–2026 progress is in **quantization**, not graph topology:

- **RaBitQ** (2024, open): randomized 1-bit quantization with a *theoretical error bound*. ~97% recall at 1 bit/dim (32× compression vs FP32). Its error bound enables *principled* reranking — you can skip reranking candidates whose lower distance bound already exceeds the best upper bound. On the MSMARCO RAG dataset, HNSW+RaBitQ delivered ~5× the QPS of plain HNSW at 99% recall with ~90% less memory (Infinity v0.6 benchmarks, Oct 2025).
- **BBQ**: 1-bit + 14-byte corrective, ~98% recall.
- **LVQ** (locality-aware): good default when accuracy ceiling matters more than extreme compression.
- **SQ8** (scalar int8): 4× compression with ~99% recall, no training — the boring, correct default.
- **4-bit + FP32 rerank** (TurboVec): native 0.88 recall → **1.000** after reranking 100 candidates, index 10× smaller, builds in <1s.

**Practical recipe for Polyvec v0.1:** build the HNSW graph at full precision (graph quality matters), traverse quantized copies (RaBitQ or SQ8), rerank top candidates against FP32/FP16 vectors. This is the exact pattern the fastest 2026 engines converged on — no new math required, just correct implementation.

### 2.3 What to take from this

- Implement **HNSW + (RaBitQ or SQ8) + exact rerank**. Do not invent a new index family.
- Keep the index **incrementally mutable** (insert/delete without rebuild) — a hard requirement for a living company KB, and the reason `instant-distance` is disqualified.

---

## 3. The knowledge-base layer above raw vectors

Raw ANN recall is the *least* differentiated part of a company KB. What determines quality in 2026:

1. **Chunking + context enrichment** — the biggest lever. Semantic-boundary chunking, section headers preserved in chunks, **contextual retrieval** (LLM-prepended context per chunk: +49–67% reported improvement), parent-child chunk retrieval (retrieve small, return large).
2. **Hybrid retrieval (dense + BM25) with RRF fusion** — the baseline, not a bonus. Dense misses exact terms ("Vitamin C", SKUs, error codes); BM25 misses paraphrase. Every major vendor (Bedrock KB, Azure AI Search, Vertex) converged on this.
3. **Cross-encoder reranking as stage 2** — +3–8 nDCG@10 over stage-1, nearly universal in production 2026 (top-10/20 reranked). Open options: `BGE-reranker-v2-m3` (568M, ~15 ms/pair, Apache-2), `mxbai-rerank-large-v2`. Cost note: at high QPS, reranker compute can exceed stage-1 ANN cost 5–20× — make it optional/tiered.
4. **Metadata filtering + ACLs at retrieval time** — tenant, team, permission tags as first-class filters (roaring bitmaps; pre-filter when selective, post-filter when broad). For a *company* KB this is non-negotiable.
5. **Temporal truth / supersession** — the thing that separates "memory" from "a pile of chunks." Facts change; the KB must model valid-intervals and supersede-not-delete. This is Supermemory's core value-add over a blank vector DB (their words: *"memory tracks facts about users over time, handles supersession, and expires stale information — while RAG retrieves document chunks"*).
6. **Entity layer** — entity identity and relations across documents. Full GraphRAG (community detection etc.) is only worth it for multi-hop/global-aggregation queries at scale; for v0.1, entity inverted indexes + link edges on top of the vector store get most of the value.
7. **Evaluation from day one** — golden Q/A set, RAGAS-style faithfulness/context metrics, CI quality gates. The teams with reliable KBs invested in the *transformation layer before anything was called a knowledge base*.

**Implication for Polyvec:** the DB should ship the ingestion pipeline (chunk → contextualize → embed → index with metadata/ACL/temporal fields), not just the ANN core. That pipeline is the product surface agents actually feel.

---

## 4. Cheap massive reads: architecture for ~zero marginal query cost

### 4.1 The techniques, quantified

| Technique | Effect | Cost |
|---|---|---|
| In-memory HNSW | p50 ~1.15 ms @ 0.96 recall (100K vectors, 8 threads) | RAM: ~3.4 KB/vec @768-dim |
| + SQ8 quantization | 4× less RAM, ~99% recall retained | negligible latency change |
| + RaBitQ (1-bit) | 32× less RAM, ~97% recall; ~5× QPS at 99% recall on RAG data | KMeans training step at build |
| mmap (vectors on NVMe, graph in RAM) | cold data without RAM blowup | some p99 tail latency |
| Batched query API | 1.3–1.8× QPS (amortizes pool + L1) | API design only |
| Multi-threaded beam search | 2–4× QPS on many-core hosts | CPU |
| Query/result cache + embedding cache | repeated agent queries → ~0 compute | RAM, invalidation logic |
| Rerank only top-K (K=10–20) | keeps reranker cost bounded | small recall tradeoff |

Decision rule of thumb (10M × 1536-dim): RAM abundant → HNSW; RAM tight → HNSW+SQ8 (75% less RAM, ~zero recall loss); single-box billions → DiskANN on NVMe.

### 4.2 The economics that validate the thesis

Pinecone serverless (2026): ~$16–18 per **million read units** (1 RU ≈ reading one record). A sustained 1000 QPS workload was estimated at **~$14,500/month in reads alone** — serverless cost is *dominated* by read volume. Meanwhile a self-hosted Qdrant node (r6i.2xlarge, 8 vCPU/64 GB) is **~$388/month flat**, regardless of QPS. Managed Postgres/pgvector runs ~$850–900/mo for the same shape.

The user's thesis — *"very cheap to make huge numbers of read calls, because agents will query the company KB constantly"* — is **validated**: metered-per-read pricing is the single most hostile cost structure for an agent-heavy workload. A fixed-cost, self-hosted, aggressively-quantized Rust binary collapses marginal query cost to electricity. The second hidden per-query tax is **embedding the query itself** ($0.02–0.13/M tokens via API) — colocating a small local embedding model removes that too.

Honest nuance: Qdrant self-hosted already captures most of this arbitrage. Polyvec's *additional* win must come from §6 (workload-shaped features), not from "cheaper than Qdrant self-hosted" alone.

---

## 5. Embeddings: 2026 practical options

| Option | Dims | Cost | Notes |
|---|---|---|---|
| **Qwen3-Embedding-0.6B** (Apache-2.0, local) | up to 1024 (MRL) | free | Best small open multilingual retrieval; 32K ctx |
| **BGE-M3** (MIT, local) | 1024 | free | Dense + sparse + ColBERT in one model — ideal for hybrid |
| **Nomic-embed-text-v1.5** (Apache-2.0, local) | 768 | free | 137M params, CPU-friendly |
| **granite-embedding-97m** (Apache-2.0, local) | 384 | free | Tiny (98 MB int8), strong multilingual |
| `text-embedding-3-small` (OpenAI API) | 1536 | $0.02/M tok | Boring reliable default |
| `voyage-3.5` (API) | 1024 | $0.06/M tok | Best commercial price/performance |
| Cohere Embed v4 (API) | 1024 | $0.12/M tok | 128K ctx, multimodal |

Trends: Matryoshka (flexible dims) and int8/binary output are standard on new models; open models have essentially closed the gap with commercial ones.

**Recommendation for Polyvec:** default to a **local** model via `ort` (ONNX) — Qwen3-0.6B for quality or BGE-M3 for built-in hybrid — with an API fallback option. Local embedding is load-bearing for the cheap-reads thesis: it removes the per-query embedding API tax and keeps the whole loop self-contained. CPU inference at ~50–100 sentences/sec is plenty for KB query traffic (one embed per query).

---

## 6. Recommendation: concrete build plan

### 6.1 v0.1 — what to build (single Rust binary, ~weeks not months)

1. **Core:** `hnsw_rs` HNSW + SQ8 (v0.1) with RaBitQ as the v0.2 quantization upgrade; full-precision graph build, quantized traversal, exact rerank of top candidates.
2. **Hybrid:** `tantivy` BM25 sidecar + RRF fusion of dense/sparse.
3. **Metadata:** `redb` payload store + `roaring-rs` filter bitmaps; pre/post-filter selection by selectivity.
4. **KB semantics:** document model with `tenant`, `acl_tags`, `valid_from/valid_to`, `supersedes` edge, `entity` tags. Ingestion pipeline: chunk (semantic boundaries) → contextualize → embed (local) → index.
5. **API:** REST + gRPC, batched query endpoint, query/result + embedding caches; **MCP server natively** (the 2026 agent interface).
6. **Eval harness:** golden Q/A set + recall/faithfulness metrics in CI from day one.

### 6.2 What NOT to build (yet)

- New ANN math or a new index family. Solved problem.
- Distributed clustering / sharding. Single node covers 10–50M vectors; add when a customer needs it.
- GPU indexing paths. CPU + quantization wins the cost game.
- A full graph database. Entity edges as metadata first; promote to graph traversal only when multi-hop queries are a measured need.
- Built-in LLM reranker training. Ship rerank as pluggable; cross-encoder optional.
- Multi-model serving infrastructure. One default local embedder, well integrated.

### 6.3 Where the real differentiation comes from

1. **Cost-model moat:** fixed-cost binary, quantized-by-default, cached, batched, local embeddings — *"the vector DB priced for agents that read 1,000×."* Against Pinecone serverless this is a 10–100× cost advantage at agent-scale read volumes; against Qdrant Cloud/self-host it's parity on cost but ahead on workload fit.
2. **Knowledge-base semantics built in:** temporal supersession, ACLs at retrieval, entity identity, contextual chunking in the ingestion path — the Supermemory feature set, self-hosted and open, without per-seat/per-call metering.
3. **Agent-native interface:** MCP-first verbs (`remember/search/forget/supersede`), designed for the Polygrow agent fleet as customer zero.
4. **Dogfood loop:** Polygrow's company KB is the reference workload — every design decision gets validated against real agent traffic, which is exactly how the cheap-reads architecture gets tuned.

### 6.4 Thesis verdict

**Validated, with one refinement.** "Cheapest possible massive reads for agents" is a real, large, structural advantage against metered SaaS (Pinecone et al.) — the economics are unambiguous. But it is *not* sufficient differentiation against Qdrant self-hosted, which already delivers cheap reads. The complete thesis should be: **cheapest reads *plus* company-KB semantics *plus* agent-native API, in one open Rust binary.** No new retrieval math is needed to get there — the win is composition, cost architecture, and workload fit.

---

### Sources (selected)

- ANN tradeoff tables: sderosiaux/performance-engineering-handbook `29-vector-ann-indexes.md`; kmohnishm/learn_basics `02_ann_algorithms`; seanpedersen.github.io arXiv benchmark post (Sep 2026)
- RaBitQ: zvec-ai/zvec-web blog (2026-09-08); vectordb-ntu/rabitq-library reranking docs; infiniflow/ragflow-docs Infinity v0.6.0 report
- Vector DB comparison 2026: dev.to enterprise comparison (Sep 2026); megaoneai.com Pinecone vs Weaviate vs Qdrant
- Cost data: dev.to enterprise cost section; linkedin.com vector DB cost estimation 2026; jatinsethi98/infinity-apple-silicon COST_COMPARISON.md
- RAG/KB quality: kaminoikari/charles-portfolio enterprise-rag-best-practices.md; prmichaelsen/remember-core rag-best-practices; lin-guanguo/llm-memory-research embedding-models
- Embeddings: mehmetaliyilmaz0/ragfactory research/embedding-models.md; madappgang/mnemex 2026 embedding paper draft; alexmost/neuro-vault 2026-08 alternatives note
- Rust ANN crates: vasovagal/vagus ADR-0019 (usearch audit); arc-labs-ai/brain-db spec/09_indexing; ractive/hyalo research
- Memory vs vector DB: supermemory.ai "AI Memory vs Vector Databases"; explainx.ai Supermemory Learner-1 analysis; vilosource/vfkb agent-memory-landscape-2026-07
