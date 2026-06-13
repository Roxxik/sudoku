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
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, KindMask, LC_CLAIMING, LC_POINTING,
    NAKED_PAIR, NAKED_QUAD, NAKED_SINGLE, NAKED_TRIPLE, NUM, SolveTrace,
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
    try_kind!(NAKED_PAIR, techniques::naked_subset(b, 2, None, None));
    try_kind!(HIDDEN_PAIR, techniques::hidden_subset(b, 2, None, None));
    try_kind!(NAKED_TRIPLE, techniques::naked_subset(b, 3, None, None));
    try_kind!(HIDDEN_TRIPLE, techniques::hidden_subset(b, 3, None, None));
    try_kind!(NAKED_QUAD, techniques::naked_subset(b, 4, None, None));
    try_kind!(HIDDEN_QUAD, techniques::hidden_subset(b, 4, None, None));
    if let Some(k) = techniques::fish_step(b, allowed, None) {
        return Some(k);
    }
    if let Some(k) = techniques::wing_step(b, allowed) {
        return Some(k);
    }
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

// --- confluence-test support --------------------------------------------------
//
// The strip loop's keep/required decisions are *fixpoint* properties (solvable? still
// stuck without the forced kind?), so they must not depend on the order the solver
// tries techniques. Singles and locked candidates are monotone (their firing condition
// survives any elimination), hence trivially confluent; the subsets/fish/wings are the
// only ones whose order could matter — the wings especially, whose bivalue/trivalue
// condition is non-monotone. `examples/confluence.rs` replays generated puzzles' solves
// under permutations of exactly this harder block and asserts the fixpoint is invariant.

/// One harder-than-closure technique group, as a value so the confluence harness can
/// reorder the ladder. Production drives [`HARD_STEPS_DEFAULT`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardStep {
    /// Naked subset of the given size (2 pair, 3 triple, 4 quad).
    NakedSubset(u8),
    /// Hidden subset of the given size.
    HiddenSubset(u8),
    /// The basic fish (X-Wing / Swordfish / Jellyfish), each gated by `allowed`.
    Fish,
    /// The bivalue wings (XY- / XYZ- / W-Wing), each gated by `allowed`.
    Wings,
}

/// The production harder-ladder order, identical to [`step_once`]'s tail: subsets by
/// ascending size (naked before hidden), then the fish, then the bivalue wings.
pub const HARD_STEPS_DEFAULT: [HardStep; 8] = [
    HardStep::NakedSubset(2),
    HardStep::HiddenSubset(2),
    HardStep::NakedSubset(3),
    HardStep::HiddenSubset(3),
    HardStep::NakedSubset(4),
    HardStep::HiddenSubset(4),
    HardStep::Fish,
    HardStep::Wings,
];

/// Apply the first firing harder step from `order`, honouring `allowed`; returns its
/// kind index, or `None` if none fire. Singles and LC are assumed already drained by the
/// caller's closure (so this is exactly `step_once`'s tail, made reorderable).
fn step_harder_ordered<V: LogicBoard>(
    b: &mut V,
    allowed: KindMask,
    order: &[HardStep],
) -> Option<usize> {
    for &step in order {
        let fired = match step {
            HardStep::NakedSubset(s) => {
                let bit = match s {
                    2 => NAKED_PAIR,
                    3 => NAKED_TRIPLE,
                    _ => NAKED_QUAD,
                };
                (allowed & (1 << bit) != 0 && techniques::naked_subset(b, s as usize, None, None))
                    .then_some(bit)
            }
            HardStep::HiddenSubset(s) => {
                let bit = match s {
                    2 => HIDDEN_PAIR,
                    3 => HIDDEN_TRIPLE,
                    _ => HIDDEN_QUAD,
                };
                (allowed & (1 << bit) != 0 && techniques::hidden_subset(b, s as usize, None, None))
                    .then_some(bit)
            }
            HardStep::Fish => techniques::fish_step(b, allowed, None),
            HardStep::Wings => techniques::wing_step(b, allowed),
        };
        if let Some(k) = fired {
            return Some(k);
        }
    }
    None
}

/// The easiest-first closure prefix below the harder block: hidden single, then the two
/// locked-candidates. Naked single is drained separately (in waves) by the caller.
/// Returns whether anything fired.
fn step_prefix<V: LogicBoard>(b: &mut V, allowed: KindMask) -> bool {
    macro_rules! try_kind {
        ($bit:expr, $call:expr) => {
            if allowed & (1 << $bit) != 0 && $call {
                return true;
            }
        };
    }
    try_kind!(HIDDEN_SINGLE, techniques::hidden_single(b));
    try_kind!(LC_POINTING, techniques::lc_pointing(b));
    try_kind!(LC_CLAIMING, techniques::lc_claiming(b));
    false
}

/// Solve `board` to the `allowed`-toolbox fixpoint with the harder ladder tried in
/// `order`, returning the final board (solved or stuck). The loop shape matches
/// [`solve_tracked`](LogicSolver::solve_tracked) — naked singles drain in waves, then
/// one prefix-or-harder step, repeat — so with `order == HARD_STEPS_DEFAULT` it reaches
/// the same fixpoint the production solver does; other orders are the confluence probe.
/// The returned board's placements + candidates ARE the fixpoint the harness compares.
pub fn solve_fixpoint_with_order<V: LogicBoard>(
    board: &V,
    allowed: KindMask,
    order: &[HardStep],
) -> V {
    let mut b = board.clone();
    let bat_singles = allowed & (1 << NAKED_SINGLE) != 0;
    loop {
        if bat_singles && drain_naked_singles(&mut b) > 0 {
            continue;
        }
        if is_solved(&b) {
            return b;
        }
        let rest = if bat_singles { allowed & !(1 << NAKED_SINGLE) } else { allowed };
        if step_prefix(&mut b, rest) {
            continue;
        }
        if step_harder_ordered(&mut b, rest, order).is_some() {
            continue;
        }
        return b; // stuck: no allowed technique fires
    }
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
