# BENCHMARKS — Methodology

> The plan's central question is stated in the thesis: **massive, cheap,
> high-recall reads**. Every number in this file must be measured,
> reproducible, and honest. Nothing below has been measured yet.

## Principles

1. **No invented numbers.** A result is either measured (with hardware,
   dataset, commit SHA, and command) or it does not exist. "Not measured
   yet" is the only acceptable placeholder.
2. **Reproducible by a stranger**: fixed seeds, pinned dependency versions,
   exact commands in `crates/polyvec-bench/` (Phase 7), raw logs committed
   alongside summaries.
3. **Cost is a first-class metric**: $/million-reads is reported next to
   latency and recall — the thesis is cost, not just speed.

## Methodology template (per benchmark)

| Field | Value |
| ----- | ----- |
| Benchmark name | (e.g. `read-recall-10m`) |
| Date | YYYY-MM-DD |
| Commit SHA | (engine commit under test) |
| Hardware | CPU model, RAM, disk type, instance type if cloud |
| Dataset | name, vector count, dimensions, source, license |
| Workload | QPS, concurrency, read/write mix, warm vs cold |
| Metric: p50/p99 latency | **not measured yet** |
| Metric: recall@k | **not measured yet** |
| Metric: $/M reads | **not measured yet** |
| Reproduce | exact command + seed |

## Planned benchmark suite (Phase 7)

- **Ingest**: vectors/sec for bulk load at 1M / 10M scale; memory peak.
- **Read latency**: p50/p99 at 1, 10, 100 concurrent readers, warm index.
- **Recall**: recall@10/recall@100 vs brute-force ground truth per index type.
- **Cost**: $/million reads on reference hardware (to be defined).
- **Regression gate**: CI fails if p99 regresses >10% vs the recorded baseline.

## Results

None. All results are **not measured yet** — Phase 0 has no engine to measure.
