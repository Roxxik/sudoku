//! xorshift64 RNG. The `next_u64` generator is `core::rng` verbatim, but
//! [`Rng::range`] uses Lemire's multiply-shift reduction instead of `% hi`, so
//! the bounded stream — and therefore the generated puzzles — intentionally
//! DIVERGE from core's (a measured speed win on the hot Fisher–Yates shuffles).
//! What is still preserved: cross-backend determinism (native == wasm32), since
//! `range` depends only on `(next_u64(), hi)` computed in the u64/u128 domain.

pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn from_seed(seed: u64) -> Self {
        // SplitMix64-finalize the seed before it becomes xorshift64 state. A directly-loaded
        // small/sequential seed avalanches too slowly: the first `next_u64()`'s high bits are
        // tiny, so the first bounded draw `range(9)` is 0 for *every* small seed (the first
        // Fisher-Yates swap is then deterministic). For the diagonal-prefill fill that dead
        // draw lands on the grid's structural backbone and pins a cell constant across all
        // sequential seeds — see `docs/FILL-BRANCH-RULE.md` §6.5. The finalizer decorrelates
        // sequential seeds in one shot, and is pure integer math so native == wasm holds.
        let mut z = seed.wrapping_add(0x9E3779B97F4A7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        // The xorshift64 state must be non-zero (zero is an absorbing fixed point); `z` is
        // zero for exactly one seed, so keep the guard.
        Self {
            state: if z == 0 { 0xDEADBEEFCAFEBABE } else { z },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    pub fn range(&mut self, hi: usize) -> usize {
        // Lemire's multiply-shift bounded reduction: one 64-bit multiply + shift
        // (the high half of a 128-bit product) instead of a modulo division. The
        // result is `floor(next_u64() * hi / 2^64)` in `[0, hi)`; its bias is
        // ~hi/2^64 (negligible for a shuffle). Computed in the u64/u128 domain so
        // it depends only on `(next_u64(), hi)` — identical on 32- and 64-bit
        // targets (native and wasm32), preserving cross-backend determinism.
        ((self.next_u64() as u128 * hi as u128) >> 64) as usize
    }

    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        let n = slice.len();
        for i in (1..n).rev() {
            let j = self.range(i + 1);
            slice.swap(i, j);
        }
    }
}
