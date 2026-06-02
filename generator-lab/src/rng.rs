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
        Self {
            state: if seed == 0 { 0xDEADBEEFCAFEBABE } else { seed },
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
