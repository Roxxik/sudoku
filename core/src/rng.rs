pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn from_seed(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0xDEADBEEFCAFEBABE } else { seed },
        }
    }

    pub fn from_entropy() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xA5A5A5A5);
        Self::from_seed(nanos.wrapping_mul(0x9E3779B97F4A7C15))
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
        // Reduce in u64 *before* narrowing to usize. `next_u64() as usize` would
        // truncate to 32 bits on wasm32 (usize = u32) but not on 64-bit hosts,
        // making the shuffle — and every generated puzzle — differ by backend.
        // Doing `% hi` in u64 keeps the result identical on 32- and 64-bit
        // targets (and is a no-op change on 64-bit, where usize is already u64).
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
