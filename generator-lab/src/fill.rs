//! Random full-grid fill — the first half of every strip attempt.
//!
//! A complete solution grid is produced by an MRV+shuffle search (the same node
//! order and RNG stream as core, so the grid is byte-identical for a given seed).
//! The search state is digit-transposed — one cell-set per digit ([`GridMask`])
//! plus an `unsolved` cell-set — instead of a per-cell candidate `Board`, which
//! makes both the MRV scan and the placement cheap and popcount-free.
//!
//! The fill is generic over the [`GridMask`] packing. Production runs the flat
//! `u128` [`FlatGridMask`]; the banded [`Bands`](crate::repr::banded) packing —
//! three 27-bit bands in one SIMD register, ~1.13x faster native (`gridbench`
//! exp H) — is a drop-in `Fill<Bands<RowMajor>>` swap, kept byte-identical by the
//! `banded_fill_matches_flat` test (and swappable once confirmed on ARM).

use crate::counters::counter_block;
use crate::repr::banded::{Bands, RowMajor};
use crate::repr::{Branchable, Digit, DigitGrid, PerDigit, Solution};
use crate::rng::Rng;
use crate::scan::{BranchStrategy, Mrv, Scan};
use std::marker::PhantomData;

// Per-node MRV min-candidate-count histogram (`feature = "count"`): slot `k` counts the
// branch nodes whose chosen cell had exactly `k` candidates. Slot 1 = naked singles — the
// fraction that decides whether a depth-2 fast path (skip the depth-4 sieve when a naked
// single exists) is worth it. Read by `fillbench` under `count`.
counter_block!(FILLSTAT: 10, inc = fillstat_inc, add = fillstat_add, snapshot = fillstat_snapshot, reset = fillstat_reset);

/// A random complete [`Solution`]. Same MRV+shuffle search as core — identical
/// grid and RNG stream for a given seed. The fill is scan-bound (~83 nodes/grid,
/// one MRV scan each, ~1.7 backtracks), so the cheap popcount-free scan over the
/// digit-transposed cell-sets is the win. Runs on the banded
/// [`Bands<RowMajor>`](crate::repr::banded) packing — the sieve fits one SIMD
/// register, so it avoids the flat `u128` sieve's GPR spill storm (~1.13x native).
/// Swap the type parameter to [`FlatGridMask`] to fall back.
pub fn random_solution(rng: &mut Rng) -> Solution {
    random_solution_with::<Mrv>(rng)
}

/// A random complete [`Solution`] under an explicit [`BranchStrategy`], on the
/// production banded rep — so strategies can be benched head-to-head (see
/// `examples/stratbench.rs`). [`random_solution`] is this with [`Mrv`].
pub fn random_solution_with<S: BranchStrategy>(rng: &mut Rng) -> Solution {
    let mut f = Fill::<Bands<RowMajor>, S>::new();
    let ok = f.fill(rng);
    debug_assert!(ok, "fill should always succeed on empty board");
    Solution(f.digits)
}

/// Digit-transposed fill state: `board[d]` holds the cells where digit `d+1` can
/// still go; `unsolved` is the cells not yet decided. A decided cell's stale bits
/// in the other boards are never cleared — they are gated out by `unsolved`
/// everywhere the scan reads them, so candidates of *unsolved* cells stay exactly
/// correct (= the board's naked candidates). `digits` records the placed digit at
/// each cell, the only thing the cell-sets can't answer, for the final grid.
struct Fill<M: Branchable, S: BranchStrategy = Mrv> {
    board: PerDigit<M>,
    unsolved: M,
    digits: DigitGrid,
    _strategy: PhantomData<S>,
}

impl<M: Branchable, S: BranchStrategy> Fill<M, S> {
    /// A fresh empty-board fill: every digit may go anywhere, every cell unsolved.
    fn new() -> Self {
        Fill {
            board: PerDigit::new([M::FULL; 9]),
            unsolved: M::FULL,
            digits: DigitGrid::EMPTY,
            _strategy: PhantomData,
        }
    }

    fn fill(&mut self, rng: &mut Rng) -> bool {
        let (cell, mask) = match S::scan(&self.board, self.unsolved) {
            Scan::Dead => return false,
            Scan::Solved => return true,
            Scan::Branch { cell, candidates } => (cell, candidates),
        };
        fillstat_add((mask.count_ones() as usize).min(9), 1);
        // Candidate digits as 0-based indices (ascending == iter_digits order) on
        // a stack array, then shuffled — same `n` elements in the same order as
        // the scalar fill, so the RNG stream and produced grid are byte-identical.
        let mut idxs = [0u8; 9];
        let mut n = 0;
        let mut m = mask;
        while m != 0 {
            idxs[n] = m.trailing_zeros() as u8;
            m &= m - 1;
            n += 1;
        }
        rng.shuffle(&mut idxs[..n]);
        // Deciding `cell` drops it from `unsolved` whichever digit wins, so do that
        // once for the whole digit loop (not per digit) and re-open it only if every
        // digit fails. Likewise forbidding `d` on `cell`'s peers touches exactly the
        // one board `board[d]`, so back up just that 16-byte mask per try instead of
        // copying all nine boards + `unsolved` (the 160-byte snapshot the loop used
        // to take every node — ~14% of fill went to those `vmovups` copies).
        let cell_mask = M::cell(cell);
        let not_peers = !M::peers(cell);
        self.unsolved &= !cell_mask;
        for &ix in &idxs[..n] {
            let d = Digit::from_index(ix as usize);
            let bu = self.board[d];
            self.board[d] &= not_peers;
            self.digits.set(cell, d);
            if self.fill(rng) {
                return true;
            }
            self.board[d] = bu;
        }
        self.unsolved |= cell_mask;
        self.digits.clear(cell);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::Fill;
    use crate::repr::banded::{Bands, RowMajor};
    use crate::repr::{FlatGridMask, PEERS};
    use crate::rng::Rng;
    use crate::scan::Bivalue;

    /// The banded `Fill<Bands<RowMajor>>` must produce byte-identical grids to the
    /// flat `Fill<FlatGridMask>` for the same seed — same MRV pick, same RNG
    /// stream. The generic-trait analogue of gridbench's exp-H fp cross-check, so
    /// the production rep can be swapped by changing one type parameter.
    #[test]
    fn banded_fill_matches_flat() {
        for seed in 0..300 {
            let mut rf = Rng::from_seed(seed);
            let mut rb = Rng::from_seed(seed);
            let mut flat = Fill::<FlatGridMask>::new();
            let mut banded = Fill::<Bands<RowMajor>>::new();
            assert!(flat.fill(&mut rf));
            assert!(banded.fill(&mut rb));
            assert_eq!(
                flat.digits.to_line(),
                banded.digits.to_line(),
                "seed {seed}"
            );
        }
    }

    /// Swapping the [`crate::scan::BranchStrategy`] type parameter to `Bivalue` must
    /// still yield a valid, complete solution (proves the strategy is a genuine
    /// drop-in, not just MRV in disguise), and it must *differ* from the `Mrv` grid
    /// for the same seed (proves the choice actually changes the search).
    #[test]
    fn bivalue_strategy_swaps_in() {
        for seed in 0..50 {
            let mut rb = Rng::from_seed(seed);
            let mut bivalue = Fill::<Bands<RowMajor>, Bivalue>::new();
            assert!(bivalue.fill(&mut rb), "bivalue fill failed, seed {seed}");
            let g = &bivalue.digits;
            assert!(g.is_complete(), "bivalue grid incomplete, seed {seed}");
            // A valid sudoku: no cell shares its digit with any peer.
            for cell in 0..81 {
                let d = g.get(cell).expect("complete");
                for &p in &PEERS[cell] {
                    assert!(g.get(p).expect("complete") != d, "peer conflict at {cell}, seed {seed}");
                }
            }
            // Different rule -> different grid than MRV (same seed).
            let mut rm = Rng::from_seed(seed);
            let mut mrv = Fill::<Bands<RowMajor>>::new();
            assert!(mrv.fill(&mut rm));
            assert_ne!(g.to_line(), mrv.digits.to_line(), "bivalue == mrv, seed {seed}");
        }
    }
}
