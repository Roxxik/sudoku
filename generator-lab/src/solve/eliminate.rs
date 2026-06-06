//! `Eliminate` — the technique layer's candidate-pruning primitive — and the
//! [`LogicBoard`] contract the logic solver reads a board through, with the bridging
//! impls that make each representation a board the engine can prune.

use crate::repr::banded::DualSolverState;
use crate::repr::{Board, CellIdx, Digit, GridMask, SolveView, SolverState};

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

/// The digit-major search board prunes a candidate by forbidding the digit at the
/// cell ([`SolverState::forbid`]) — the same op the prober uses as its uniqueness
/// lever, here as the technique engine's elimination. Generic over the packing, so
/// the logic solver runs on the flat reference and either banding.
impl<M: GridMask> Eliminate for SolverState<M> {
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
/// ([`DualSolverState::forbid`]) — so the composable techniques (and the fused
/// engine's subset ladder) run on it directly, every unit in-lane in one view.
impl Eliminate for DualSolverState {
    #[inline]
    fn eliminate(&mut self, cell: CellIdx, d: Digit) {
        self.forbid(cell, d);
    }
}
