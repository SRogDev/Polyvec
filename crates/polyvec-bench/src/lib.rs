//! Benchmark harness support: deterministic fixtures and measurement helpers.
//!
//! Every benchmark derives its inputs from fixed seeds, so any run is
//! reproducible. Nothing here is a claim about production quality — see
//! `BENCHMARKS.md` for what each number does and does not mean.

pub mod fixtures;
pub mod measure;
