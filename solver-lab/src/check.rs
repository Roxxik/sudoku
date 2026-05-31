//! Generic prober-vs-oracle correctness check, shared by the registry.
//!
//! Lives in `src` (not `tests`) so [`crate::solvers::REGISTRY`] can hold a
//! `check` fn pointer per variant — that is what lets `tests/correctness.rs`
//! verify every registered prober by iterating the registry, with no per-variant
//! edit. It only uses the lab's own [`oracle`](crate::oracle) (no `sudoku-core`),
//! so it is safe to compile into the library.

use crate::grid::{Board, CELLS, iter_digits};
use crate::oracle::count_solutions;
use crate::rng::Rng;
use crate::solvers::UniqProber;

/// Build a random, internally-consistent partial board by placing `givens`
/// legal digits (skipping illegal picks).
fn random_partial(rng: &mut Rng, givens: usize) -> Board {
    let mut b = Board::empty();
    for _ in 0..givens {
        let cell = rng.range(CELLS);
        if !b.is_empty(cell) {
            continue;
        }
        let cs = b.candidates(cell);
        if cs == 0 {
            continue;
        }
        let choices: Vec<u8> = iter_digits(cs).collect();
        let d = choices[rng.range(choices.len())];
        b.place(cell, d);
    }
    b
}

/// Assert prober `P` agrees with the canonical backtracker, mirroring the
/// strip-loop usage: clear a filled cell, build the probe, and check
/// `has_solution_with` against `count_solutions` for every alternate digit.
/// Panics on the first mismatch.
pub fn check_prober<P: UniqProber>() {
    let mut rng = Rng::from_seed(0x1234_5678);
    for _ in 0..150 {
        let givens = rng.range(50) + 5;
        let mut b = random_partial(&mut rng, givens);
        let filled: Vec<usize> = (0..CELLS).filter(|&i| !b.is_empty(i)).collect();
        if filled.is_empty() {
            continue;
        }
        let i = filled[rng.range(filled.len())];
        b.clear_naked(i);
        let mut probe = P::from_board(&b);
        for d in iter_digits(b.candidates(i)) {
            let probe_says = probe.has_solution_with(i, d);
            let mut placed = b.clone();
            placed.place(i, d);
            let canonical = count_solutions(&placed, 1) > 0;
            assert_eq!(
                probe_says,
                canonical,
                "{} mismatch at cell {} digit {} on {}",
                P::NAME,
                i,
                d,
                b.to_line()
            );
        }
    }
}
