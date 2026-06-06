//! `Marks` — the candidate-store abstraction: per cell, the set of digits it may
//! still take, packed however the impl chooses. The candidate counterpart of
//! [`GridMask`](super::GridMask) (which abstracts a bare cell-set): `Marks`
//! abstracts the *whole* candidate state, so a prober, baseline solver, or strip
//! loop can derive, maintain, and read candidates without committing to a packing
//! — swapping packing becomes a change of type parameter, exactly as `GridMask`
//! does for the fill.
//!
//! Implemented by the cell-major scalar [`MarkGrid`](super::MarkGrid) (`[Mark; 81]`,
//! the reference), the [`Board`](super::Board), the digit-major
//! [`SolverState<M>`](super::SolverState) (the search/prober board, any packing),
//! and the dual-view [`DualSolverState`](super::banded::DualSolverState) (the
//! baseline's — every unit in-lane in one of its two views). A `from_digits`-derived
//! grid in any packing holds the same candidates cell for cell; the
//! `marks_agree_across_packings` test pins that.
//!
//! Scope: this is the *representation* contract — derive, place, read — and
//! nothing more. It is deliberately NOT the solver-facing interface:
//! emptiness/`unsolved` and the unit-level scans the technique engine needs are
//! not here, because they are not uniformly answerable from candidates alone (the
//! scalar `MarkGrid` cannot tell a filled cell from a contradicted one without its
//! placements; the banded grids carry an explicit `unsolved` mask). Those land on
//! a richer, solver-facing trait when the technique layer does.

use super::{CellIdx, Digit, DigitGrid, Mark};

/// A per-cell candidate store, abstract over packing. The invariant every impl
/// upholds: after [`from_digits`](Marks::from_digits) and any sequence of
/// [`place`](Marks::place)s, [`get`](Marks::get) returns each empty cell's naked
/// candidates (the digits no filled peer rules out) and the empty set for a filled
/// cell — identical across packings for the same placements.
pub trait Marks {
    /// Derive the candidates implied by a placement grid: every empty cell starts
    /// with all digits, then each placed digit leaves its peers. The constructor
    /// counterpart of the incremental [`place`](Marks::place).
    fn from_digits(grid: &DigitGrid) -> Self
    where
        Self: Sized;

    /// Maintain the candidates for placing digit `d` at `cell`: the cell takes no
    /// further candidates and `d` leaves every peer.
    fn place(&mut self, cell: CellIdx, d: Digit);

    /// The naked candidate set of `cell` — its remaining digits if empty, the
    /// empty set if filled. The candidate analogue of
    /// [`DigitGrid::get`](super::DigitGrid::get).
    fn get(&self, cell: CellIdx) -> Mark;
}

#[cfg(test)]
mod tests {
    use super::Marks;
    use crate::repr::banded::DualSolverState;
    use crate::repr::{CELLS, Digit, DigitGrid, MarkGrid};

    /// A mid-solve grid (givens + space across all three bands), the same fixture
    /// the per-impl banded tests use.
    const GRID: &str = "\
        53..7....\
        6..195...\
        .98....6.\
        8...6...3\
        4..8.3..1\
        7...2...6\
        .6....28.\
        ...419..5\
        ....8..79";

    /// Generic over any packing: derive from a grid, place one digit, and assert
    /// every cell's candidates equal the scalar `MarkGrid` reference. Proves a
    /// `Marks` impl is a faithful, swappable candidate store — the candidate
    /// analogue of fill's `banded_fill_matches_flat`.
    fn agrees_with_reference<M: Marks>() {
        let grid = DigitGrid::parse(GRID).unwrap();
        let mut subject = M::from_digits(&grid);
        let mut reference = MarkGrid::from_digits(&grid);

        // Cell 2 (row 0, col 2) is empty in GRID; place a digit it admits.
        let cell = 2;
        let d = Digit::new(1).unwrap();
        assert!(reference.get(cell).contains(d));
        subject.place(cell, d);
        reference.place(cell, d);

        for c in 0..CELLS {
            assert_eq!(subject.get(c), reference.get(c), "cell {c}");
        }
    }

    #[test]
    fn marks_agree_across_packings() {
        agrees_with_reference::<MarkGrid>();
        agrees_with_reference::<DualSolverState>();
    }
}
