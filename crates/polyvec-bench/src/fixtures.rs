//! Deterministic fixtures: seeded PRNG, synthetic vectors, synthetic text.
//!
//! The PRNG is xorshift64* — tiny, reproducible, dependency-free. Seeds are
//! fixed per benchmark and recorded in `BENCHMARKS.md`.

/// Deterministic xorshift64* PRNG.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Seeds the generator. Seed `0` is remapped (xorshift collapses on 0).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// Next `u64`.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        self.0 = x;
        x
    }

    /// Next `f32` in `[0, 1)`.
    #[allow(clippy::cast_precision_loss)] // fixture RNG: the recipe, not a measurement
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 11) as f32 / (1u64 << 53) as f32
    }

    /// Next `usize` in `0..bound`.
    // Fixture RNG: `bound` is always a small table length, far below `u32::MAX`.
    #[allow(clippy::cast_possible_truncation)]
    pub fn next_below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// `n` L2-normalized random vectors of `dim`, deterministically from `seed`.
#[must_use]
#[allow(clippy::cast_precision_loss)] // fixture RNG
pub fn unit_vectors(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut rng = Rng::new(seed);
    (0..n)
        .map(|_| {
            let mut v: Vec<f32> = (0..dim).map(|_| rng.next_f32() * 2.0 - 1.0).collect();
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            for x in &mut v {
                *x /= norm;
            }
            v
        })
        .collect()
}

/// A small fixed vocabulary of distinctive topic words. Document `i` is
/// built around `TOPICS[i % TOPICS.len()]`, repeated so each document has
/// a recognizable signature.
const TOPICS: &[&str] = &[
    "photosynthesis",
    "blockchain",
    "sourdough",
    "kubernetes",
    "mycorrhizae",
    "tokamak",
    "ukulele",
    "cryptography",
    "permaculture",
    "quasar",
];

const FILLER: &[&str] = &[
    "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "and", "then", "runs",
    "through", "green", "fields", "under", "pale", "morning", "light", "while", "birds", "sing",
    "soft", "songs", "of", "distant", "rivers", "winding", "past", "old", "stone", "bridges",
];

/// Synthetic knowledge corpus: `(source, text)` pairs. Each document is
/// several paragraphs revolving around one distinctive topic word, so a
/// query made of that topic word has a known-relevant document.
///
/// Deterministic from `seed`. **Not** a semantic-quality fixture: with the
/// deterministic stub provider, only exact-text self-queries are meaningful
/// (see `BENCHMARKS.md`).
#[must_use]
pub fn synthetic_corpus(n_docs: usize, seed: u64) -> Vec<(String, String)> {
    use std::fmt::Write as _;
    let mut rng = Rng::new(seed);
    (0..n_docs)
        .map(|i| {
            let topic = TOPICS[i % TOPICS.len()];
            let mut text = format!("# Document {i}: {topic}\n\n");
            for para in 0..6 {
                write!(text, "Paragraph {para} about {topic}. ").expect("write to String");
                for _ in 0..40 {
                    let w = FILLER[rng.next_below(FILLER.len())];
                    text.push_str(w);
                    text.push(' ');
                }
                // The topic word recurs so the document is "about" it.
                write!(text, "In summary, {topic} matters here. ").expect("write to String");
                text.push('\n');
            }
            (format!("bench://doc-{i:04}"), text)
        })
        .collect()
}
