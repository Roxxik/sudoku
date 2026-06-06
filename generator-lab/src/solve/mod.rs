//! `solve` — the **logic solver**: a technique-driven engine that solves a puzzle
//! by applying human deduction techniques to a fixpoint, **never backtracking**.
//! The counterpart of [`probe`](crate::probe): a prober *searches* (it branches and
//! guesses to count completions); a [`LogicSolver`] only *deduces* (it places what
//! the allowed toolbox forces and stops when nothing more is forced). It is the spec
//! oracle — the strip loop's difficulty gate — not a completion oracle.
//!
//! It was once called the `baseline` engine, renamed because "baseline" said nothing
//! about *what* it is. It exposes two spec primitives:
//!
//! - [`Solver::solve_tracked`]: solve with the `allowed` toolbox, easiest-first,
//!   reporting whether it [`solved`](SolveTrace::solved) and how many times each kind
//!   fired — the strip's solvability gate and requirement counter in one pass.
//! - [`Solver::min_target_uses`]: verify's avoid-target walk — how many times a
//!   `target` technique is *forced* given the rest of the `scope` toolbox.
//!
//! Like the prober, the engine is a swap point behind a trait so the bench and the
//! strip read `S: Solver<…>` rather than a concrete type. The one engine here today
//! is [`LogicSolver`], the **composable** reference: the full ladder
//! ([`techniques`]) written once over any [`LogicBoard`], packing-agnostic and the
//! kept default — exactly the role [`probe::Singles`](crate::probe::Singles) plays
//! for the prober. A fused per-band fast path (the shape of bb's `band_update`) can
//! later slot in behind the same `Solver` surface; the composable form stays the
//! fallback and the correctness oracle.

mod fused;
mod techniques;

pub use fused::FusedLogicSolver;

use crate::repr::banded::DualBandedMarkGrid;
use crate::repr::{Board, CELLS, CellIdx, Digit, GridMask, SearchState, SolveView};
use crate::technique_kinds::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, KindMask, LC_CLAIMING, LC_POINTING,
    NAKED_PAIR, NAKED_QUAD, NAKED_SINGLE, NAKED_TRIPLE, NUM, SolveTrace,
};

/// Remove a candidate without placing — the technique layer's pruning primitive,
/// the one mutation the representation's [`Marks`](crate::repr::Marks) contract
/// deliberately leaves out (it carries only derive/place/read). Locked candidates
/// and subsets prune candidates without deciding a cell, so the logic solver needs
/// it; the prober never does (it only places and branches), which is why this lives
/// here, with the technique engine, rather than on `Marks`.
pub trait Eliminate {
    /// Remove digit `d` from `cell`'s candidates. The cell stays empty.
    fn eliminate(&mut self, cell: CellIdx, d: Digit);
}

/// A board the logic solver can read and prune: a [`SolveView`] (candidates +
/// occupancy + clone) that also supports candidate [`Eliminate`]. The blanket impl
/// makes every qualifying type a `LogicBoard` automatically, so a technique body
/// reads `V: LogicBoard` rather than the full pair of bounds.
pub trait LogicBoard: SolveView + Eliminate {}
impl<T: SolveView + Eliminate> LogicBoard for T {}

/// A technique-driven solver over board `B` — the swap point between engines, so the
/// strip loop and the bench commit to a capability, not a concrete type. Stateless:
/// the methods are associated functions on a marker type, mirroring
/// [`Prober`](crate::probe::Prober).
pub trait Solver<B> {
    /// Solve `board` with the `allowed` toolbox, easiest-first, tallying which kinds
    /// fired. `solved` and the per-kind counts are what the spec gate and requirement
    /// check read.
    fn solve_tracked(board: &B, allowed: KindMask) -> SolveTrace;

    /// The minimum number of times a `target` technique is *forced* to fire to solve
    /// `board` given the `scope` toolbox: prefer any non-target in-scope step, take a
    /// target step only when stuck on non-targets, counting those. `>= required`
    /// means the target is irreplaceable by the rest of `scope`.
    fn min_target_uses(board: &B, scope: KindMask, target: KindMask) -> usize;
}

/// The composable logic solver: drives the full discrete technique ladder
/// ([`techniques`]) over any [`LogicBoard`], easiest-first. The reference engine —
/// correct, packing-agnostic, and the kept default; the analogue of
/// [`probe::Singles`](crate::probe::Singles).
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

/// Apply the easiest in-`allowed` technique that fires, returning its kind index, or
/// `None` if none apply (stuck). Difficulty order — the same ladder the scalar
/// `step_once` and bb's `step_harder` walk.
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

/// The digit-major search board prunes a candidate by forbidding the digit at the
/// cell ([`SearchState::forbid`]) — the same op the prober uses as its uniqueness
/// lever, here as the technique engine's elimination. Generic over the packing, so
/// the logic solver runs on the flat reference and either banding.
impl<M: GridMask> Eliminate for SearchState<M> {
    #[inline]
    fn eliminate(&mut self, cell: CellIdx, d: Digit) {
        self.forbid(cell, d);
    }
}

/// The cell-major [`Board`] prunes a candidate straight off its [`MarkGrid`]
/// ([`Board::eliminate`]) — so the composable logic solver runs on the scalar working
/// board too. This is the fast surface for the cold verify path: the technique scans
/// read candidates per cell in O(1) (a [`Mark`](crate::repr::Mark) load) rather than the
/// digit-major board's 9-board scan per `get`.
impl Eliminate for Board {
    #[inline]
    fn eliminate(&mut self, cell: CellIdx, d: Digit) {
        self.eliminate(cell, d);
    }
}

/// The dual-banded grid prunes by forbidding the digit in both views at once
/// ([`DualBandedMarkGrid::forbid`]) — so the composable techniques (and the fused
/// engine's subset ladder) run on it directly, every unit in-lane in one view.
impl Eliminate for DualBandedMarkGrid {
    #[inline]
    fn eliminate(&mut self, cell: CellIdx, d: Digit) {
        self.forbid(cell, d);
    }
}

#[cfg(test)]
mod tests {
    use super::{LogicSolver, Solver};
    use crate::repr::banded::{Bands, RowMajor};
    use crate::repr::{DigitGrid, FlatGridMask, Marks, SearchState};
    use crate::spec_for_mode;

    type Banded = SearchState<Bands<RowMajor>>;

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

    fn state<M: crate::repr::GridMask>(s: &str) -> SearchState<M> {
        SearchState::from_digits(&DigitGrid::parse(s).unwrap())
    }

    /// The classic singles puzzle solves with just the two singles, and the verdict
    /// is independent of packing (flat vs banded reach the same fixpoint).
    #[test]
    fn solves_singles_puzzle() {
        let baseline = spec_for_mode(0).baseline_mask();
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
        let baseline = spec_for_mode(0).baseline_mask();
        let empty: Banded = state(&".".repeat(81));
        assert!(!LogicSolver::solve_tracked(&empty, baseline).solved);
    }
}
