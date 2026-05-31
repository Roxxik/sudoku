//! Shared bench loop, used by both the native example (`examples/bench.rs`,
//! timed with `Instant`) and the JS-callable wasm exports (timed with
//! `performance.now()` under node/V8 or a browser). Keeping one loop means
//! native and wasm measure exactly the same work.

use crate::generate::strip_attempt;
use crate::grid::CELLS;
use crate::rng::Rng;
use crate::solvers::UniqProber;

/// Run `attempts` strip attempts with prober `P` from a fresh RNG seeded
/// `seed`. Returns `(fingerprint, total_givens)`.
///
/// The fingerprint is an FNV-1a fold over every produced puzzle's cells — it
/// both prevents the optimizer from discarding the work and lets callers verify
/// that all provers walked the identical (correct) trajectory. It folds over
/// `cell(i)` directly (no `to_line` allocation) so the per-attempt overhead
/// outside the solver stays minimal and doesn't dilute the comparison.
pub fn fingerprint_run<P: UniqProber>(attempts: usize, seed: u64) -> (u64, usize) {
    let mut rng = Rng::from_seed(seed);
    let mut fingerprint: u64 = 0xcbf29ce484222325;
    let mut total_givens = 0usize;
    for _ in 0..attempts {
        let attempt = strip_attempt::<P>(&mut rng);
        for i in 0..CELLS {
            fingerprint ^= attempt.puzzle.cell(i) as u64;
            fingerprint = fingerprint.wrapping_mul(0x100000001b3);
            if attempt.puzzle.cell(i) != 0 {
                total_givens += 1;
            }
        }
    }
    (fingerprint, total_givens)
}
