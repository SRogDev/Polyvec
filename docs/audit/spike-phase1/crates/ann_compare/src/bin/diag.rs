//! Diagnostic: hnsw_rs recall vs ef_search, DistCosine vs DistL2, small scale.
use hnsw_rs::prelude::*;
use std::collections::HashSet;
use std::time::Instant;

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
    // ground truth
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

    for (m, efc) in [(16, 200), (32, 200), (16, 100)] {
        let hnsw = Hnsw::<f32, DistCosine>::new(m, N + 100, 16, efc, DistCosine {});
        let t = Instant::now();
        let refs: Vec<(&Vec<f32>, usize)> = data.iter().enumerate().map(|(i, v)| (v, i)).collect();
        hnsw.parallel_insert(&refs);
        println!("M={m} efC={efc}: insert {:?} ", t.elapsed());
        for ef in [16usize, 64, 200, 500] {
            let mut r = 0.0;
            for qi in 0..NQ {
                let res = hnsw.search(&queries[qi], K, ef);
                let ids: Vec<usize> = res.iter().map(|n| n.get_origin_id()).collect();
                r += recall(&ids, &truth[qi]);
            }
            println!("  ef_search={ef}: recall@10 = {:.4}", r / NQ as f32);
        }
    }
    // DistL2 sanity
    let hnsw = Hnsw::<f32, DistL2>::new(16, N + 100, 16, 200, DistL2 {});
    let refs: Vec<(&Vec<f32>, usize)> = data.iter().enumerate().map(|(i, v)| (v, i)).collect();
    hnsw.parallel_insert(&refs);
    let mut r = 0.0;
    for qi in 0..NQ {
        let res = hnsw.search(&queries[qi], K, 200);
        let ids: Vec<usize> = res.iter().map(|n| n.get_origin_id()).collect();
        r += recall(&ids, &truth[qi]);
    }
    println!("DistL2 ef=200: recall@10 = {:.4}", r / NQ as f32);
}
