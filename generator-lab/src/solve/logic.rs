//! `LogicSolver` — the **composable** reference logic solver: drives the full
//! discrete technique ladder ([`techniques`](super::techniques)) over any
//! [`LogicBoard`], easiest-first, never backtracking. Correct, packing-agnostic, and
//! the kept default — the analogue of [`probe::Singles`](crate::probe::Singles) for
//! the prober. The fused per-band fast path [`FusedLogicSolver`](super::FusedLogicSolver)
//! slots in behind the same [`Solver`] surface; this stays the fallback and the
//! correctness oracle.

use super::{LogicBoard, Solver, techniques};
use crate::repr::CELLS;
use crate::spec::kinds::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, JELLYFISH, KindMask, LC_CLAIMING,
    LC_POINTING, NAKED_PAIR, NAKED_QUAD, NAKED_SINGLE, NAKED_TRIPLE, NUM, SWORDFISH, SolveTrace,
    W_WING, X_WING, XYZ_WING, XY_WING,
};

/// The composable logic solver: drives the full discrete technique ladder
/// ([`techniques`](super::techniques)) over any [`LogicBoard`], easiest-first. The
/// reference engine — correct, packing-agnostic, and the kept default; the analogue
/// of [`probe::Singles`](crate::probe::Singles).
pub struct LogicSolver;

impl<V: LogicBoard> Solver<V> for LogicSolver {
    fn solve_tracked(board: &V, allowed: KindMask) -> SolveTrace {
        let mut b = board.clone();
        let mut counts = [0u16; NUM];
        let bat_singles = allowed & (1 << NAKED_SINGLE) != 0;
        loop {
            // Naked singles drain in batched waves (the overwhelmingly most common
            // step, and the cascade after every harder elimination) instead of one
            // per full ladder scan. This only reorders placements within the naked-
            // single band, so it is confluent with the easiest-first trace.
            if bat_singles {
                let n = drain_naked_singles(&mut b);
                if n > 0 {
                    counts[NAKED_SINGLE] = counts[NAKED_SINGLE].saturating_add(n);
                    continue;
                }
            }
            if is_solved(&b) {
                return SolveTrace { solved: true, counts };
            }
            // Mask naked single off once drained — a fresh full scan for it is
            // guaranteed to find nothing.
            let rest = if bat_singles { allowed & !(1 << NAKED_SINGLE) } else { allowed };
            match step_once(&mut b, rest) {
                Some(idx) => counts[idx] = counts[idx].saturating_add(1),
                None => return SolveTrace { solved: false, counts },
            }
        }
    }

    fn min_target_uses(board: &V, scope: KindMask, target: KindMask) -> usize {
        let mut b = board.clone();
        let non_target = scope & !target;
        let mut count = 0usize;
        loop {
            if is_solved(&b) {
                return count;
            }
            if step_once(&mut b, non_target).is_some() {
                continue;
            }
            // Stuck on non-targets: the only steps `scope` adds are target ones.
            match step_once(&mut b, scope) {
                None => return count, // stuck entirely
                Some(idx) => {
                    if target & (1 << idx) != 0 {
                        count += 1;
                    }
                }
            }
        }
    }
}

/// Apply the first in-`allowed` technique that fires, returning its kind index, or
/// `None` if none apply (stuck). The try-order below is THIS engine's choice (it
/// happens to follow core's order) — the kind index does not dictate it, and other
/// engines may order differently. NOTE: the fish ladder order is an un-optimized
/// placeholder (core's order); it is not benchmarked yet. Branch-scoped specs never
/// have both subsets and fish in scope at once, so their relative order is moot for
/// production; only an all-techniques baseline would see it. The same holds for the
/// bivalue wings (XY-/XYZ-/W-Wing), appended last.
fn step_once<V: LogicBoard>(b: &mut V, allowed: KindMask) -> Option<usize> {
    macro_rules! try_kind {
        ($bit:expr, $call:expr) => {
            if allowed & (1 << $bit) != 0 && $call {
                return Some($bit);
            }
        };
    }
    try_kind!(NAKED_SINGLE, techniques::naked_single(b));
    try_kind!(HIDDEN_SINGLE, techniques::hidden_single(b));
    try_kind!(LC_POINTING, techniques::lc_pointing(b));
    try_kind!(LC_CLAIMING, techniques::lc_claiming(b));
    try_kind!(NAKED_PAIR, techniques::naked_subset(b, 2));
    try_kind!(HIDDEN_PAIR, techniques::hidden_subset(b, 2));
    try_kind!(NAKED_TRIPLE, techniques::naked_subset(b, 3));
    try_kind!(HIDDEN_TRIPLE, techniques::hidden_subset(b, 3));
    try_kind!(NAKED_QUAD, techniques::naked_subset(b, 4));
    try_kind!(HIDDEN_QUAD, techniques::hidden_subset(b, 4));
    try_kind!(X_WING, techniques::fish(b, 2));
    try_kind!(SWORDFISH, techniques::fish(b, 3));
    try_kind!(JELLYFISH, techniques::fish(b, 4));
    try_kind!(XY_WING, techniques::xy_wing(b));
    try_kind!(XYZ_WING, techniques::xyz_wing(b));
    try_kind!(W_WING, techniques::w_wing(b));
    None
}

/// Place every naked single in repeated waves until none remain; return how many
/// were placed. Placing during a sweep lets a single created later in the same pass
/// be caught immediately; the outer loop catches ones created earlier.
fn drain_naked_singles<V: LogicBoard>(b: &mut V) -> u16 {
    let mut placed = 0u16;
    loop {
        if !techniques::naked_single(b) {
            return placed;
        }
        placed = placed.saturating_add(1);
        // naked_single placed exactly one; re-scan from the top for cascades.
    }
}

/// Whether every cell is filled — the solved verdict the easiest-first loop checks
/// once no naked single drains.
#[inline]
fn is_solved<V: LogicBoard>(b: &V) -> bool {
    (0..CELLS).all(|c| b.is_occupied(c))
}

#[cfg(test)]
mod tests {
    use crate::repr::banded::{Bands, RowMajor};
    use crate::repr::{DigitGrid, FlatGridMask, Marks, SolverState};
    use crate::solve::{LogicSolver, Solver};
    use crate::subset_spec_for_mode;

    type Banded = SolverState<Bands<RowMajor>>;

    const PUZZLE: &str = "\
        53..7....\
        6..195...\
        .98....6.\
        8...6...3\
        4..8.3..1\
        7...2...6\
        .6....28.\
        ...419..5\
        ....8..79";

    fn state<M: crate::repr::GridMask>(s: &str) -> SolverState<M> {
        SolverState::from_digits(&DigitGrid::parse(s).unwrap())
    }

    /// The classic singles puzzle solves with just the two singles, and the verdict
    /// is independent of packing (flat vs banded reach the same fixpoint).
    #[test]
    fn solves_singles_puzzle() {
        let baseline = subset_spec_for_mode(0).baseline_mask();
        let flat = LogicSolver::solve_tracked(&state::<FlatGridMask>(PUZZLE), baseline);
        let band = LogicSolver::solve_tracked(&state::<Bands<RowMajor>>(PUZZLE), baseline);
        assert!(flat.solved, "did not solve under train baseline");
        assert_eq!(flat.solved, band.solved);
        assert_eq!(flat.counts, band.counts, "packing changed the trace");
    }

    /// A board that is not solvable by the toolbox reports `solved == false` rather
    /// than looping — the empty grid has many completions, so singles stall.
    #[test]
    fn unsolvable_stops() {
        let baseline = subset_spec_for_mode(0).baseline_mask();
        let empty: Banded = state(&".".repeat(81));
        assert!(!LogicSolver::solve_tracked(&empty, baseline).solved);
    }
}
