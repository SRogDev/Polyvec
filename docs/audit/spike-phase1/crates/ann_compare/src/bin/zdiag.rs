//! Diagnostic: zvec-rs recall across MetricType::{Cosine, L2, IP}, N=2000.
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use zvec_rs::{Collection, CollectionConfig, HnswParams, MetricType};

struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 11) as f32 / (1u64 << 53) as f32
    }
}

fn main() {
    const N: usize = 2000;
    const DIM: usize = 128;
    const NQ: usize = 50;
    const K: usize = 10;
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    let gen = |rng: &mut Rng| -> Vec<f32> {
        let mut v: Vec<f32> = (0..DIM).map(|_| rng.next_f32() * 2.0 - 1.0).collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut v {
            *x /= n;
        }
        v
    };
    let data: Vec<Vec<f32>> = (0..N).map(|_| gen(&mut rng)).collect();
    let queries: Vec<Vec<f32>> = (0..NQ).map(|_| gen(&mut rng)).collect();
    let truth: Vec<Vec<usize>> = queries
        .iter()
        .map(|q| {
            let mut s: Vec<(usize, f32)> = data
                .iter()
                .enumerate()
                .map(|(i, v)| (i, v.iter().zip(q.iter()).map(|(a, b)| a * b).sum::<f32>()))
                .collect();
            s.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            s.truncate(K);
            s.into_iter().map(|(i, _)| i).collect()
        })
        .collect();
    let recall = |ids: &[usize], ex: &[usize]| -> f32 {
        let set: HashSet<usize> = ex.iter().cloned().collect();
        ids.iter().filter(|i| set.contains(i)).count() as f32 / ex.len() as f32
    };
    for metric in [MetricType::Cosine, MetricType::L2, MetricType::IP] {
        let cfg = CollectionConfig::new(DIM)
            .with_metric(metric)
            .with_hnsw_params(HnswParams::new(16, 200));
        let col = Collection::new(cfg); // in-memory, no persistence
        let t = Instant::now();
        for (i, v) in data.iter().enumerate() {
            col.upsert(&format!("doc-{i}"), v, HashMap::new());
        }
        println!("{metric:?}: insert {N}: {:?}", t.elapsed());
        let mut r = 0.0;
        for qi in 0..NQ {
            let hits = col.search(&queries[qi], K, None).unwrap();
            let ids: Vec<usize> = hits
                .iter()
                .map(|h| h.pk["doc-".len()..].parse::<usize>().unwrap())
                .collect();
            r += recall(&ids, &truth[qi]);
        }
        println!("{metric:?}: recall@10 = {:.4}", r / NQ as f32);
        // exact-vector sanity: query with doc-42's own vector
        let hits = col.search(&data[42], 3, None).unwrap();
        println!(
            "  exact-match doc-42 rank-1: {}",
            hits.first().map(|h| h.pk.as_str()).unwrap_or("none")
        );
    }
}
