//! xorshift64 RNG, copied verbatim from `core::rng` so the lab's generator
//! reproduces core's puzzle stream for a given seed.

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
        // Reduce in u64 before narrowing (matches core::rng — keeps the stream
        // identical across 32/64-bit targets).
        (self.next_u64() % hi as u64) as usize
    }

    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        let n = slice.len();
        for i in (1..n).rev() {
            let j = self.range(i + 1);
            slice.swap(i, j);
        }
    }
}
