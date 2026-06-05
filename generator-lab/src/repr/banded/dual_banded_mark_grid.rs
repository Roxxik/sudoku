//! `DualBandedMarkGrid` — the candidates in *both* bandings at once: a row-major
//! [`SearchState`] (rows and boxes in-lane) and a column-major one (columns and
//! boxes in-lane), kept consistent at every mutation. The banded analogue of the
//! scalar [`MarkGrid`](super::super::MarkGrid) for the *baseline technique engine*,
//! mirroring the old dual-view `BitBoard`.
//!
//! ## Why two views
//!
//! A unit is cheap to scan only when it is *in-lane* — a contiguous run inside one
//! 27-bit band. Row-major puts rows and boxes in-lane but columns straddle all
//! three bands; column-major is the transpose. Holding both copies puts *every*
//! unit in-lane in at least one view, so the engine never has to scan a unit that
//! crosses bands. The lean prober needs only one view (it reaches columns by
//! branching, not by scanning), so it uses a single
//! [`SearchState<Bands<RowMajor>>`]; the baseline genuinely needs every unit and
//! pays for the second copy.
//!
//! Placements sync for free — each view clears its own peer mask — so maintaining
//! the pair is just doing the single-view mutation twice, once per banding. The
//! engine that *reads* the two views to run techniques lives a layer up.

use super::banding::{ColMajor, RowMajor};
use super::bands::Bands;
use crate::repr::{CellIdx, Digit, DigitGrid, Mark, Marks, Occupancy, SearchState};

/// Candidates held in both bandings, kept consistent. The two views always encode
/// the same candidate set (transposed), so reads answer from the row-major copy;
/// mutations touch both.
#[derive(Clone, PartialEq, Eq)]
pub struct DualBandedMarkGrid {
    row: SearchState<Bands<RowMajor>>,
    col: SearchState<Bands<ColMajor>>,
}

impl Occupancy for DualBandedMarkGrid {
    /// Answered from the row view's `unsolved` mask (both views agree).
    fn is_empty(&self, cell: CellIdx) -> bool {
        self.row.is_empty(cell)
    }
}

impl Marks for DualBandedMarkGrid {
    /// Derive both views from a digit grid — the same per-view derivation run once
    /// per banding.
    fn from_digits(grid: &DigitGrid) -> Self {
        DualBandedMarkGrid {
            row: SearchState::from_digits(grid),
            col: SearchState::from_digits(grid),
        }
    }

    /// Place digit `d` at `cell` in both views: decide the cell and forbid `d` on
    /// its peers in each banding. The peer masks are per-view, so the two stay in
    /// sync with no cross-view transpose.
    fn place(&mut self, cell: CellIdx, d: Digit) {
        self.row.place(cell, d);
        self.col.place(cell, d);
    }

    /// The naked candidate set of cell `cell`. Both views agree, so this reads the
    /// row-major copy.
    fn get(&self, cell: CellIdx) -> Mark {
        self.row.get(cell)
    }
}

#[cfg(test)]
mod tests {
    use super::DualBandedMarkGrid;
    use crate::repr::{CELLS, Digit, DigitGrid, MarkGrid, Marks};

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

    /// Both views must agree with each other AND match the scalar `MarkGrid`, on
    /// derivation and after an incremental `place`. Checking the column view (not
    /// just the row view `get` exposes) exercises the column-major banding and its
    /// peer masks.
    #[test]
    fn both_views_match_scalar_through_place() {
        let check = |dual: &DualBandedMarkGrid, scalar: &MarkGrid| {
            for cell in 0..CELLS {
                let s = scalar.get(cell);
                assert_eq!(dual.row.get(cell), s, "row cell {cell}");
                assert_eq!(dual.col.get(cell), s, "col cell {cell}");
            }
        };

        let grid = DigitGrid::parse(GRID).unwrap();
        let mut dual = DualBandedMarkGrid::from_digits(&grid);
        let mut scalar = MarkGrid::from_digits(&grid);
        check(&dual, &scalar);

        // Cell 2 (row 0, col 2) is empty in GRID; place a digit it admits.
        let cell = 2;
        let d = Digit::new(1).unwrap();
        assert!(scalar.get(cell).contains(d));
        dual.place(cell, d);
        scalar.place(cell, d);
        check(&dual, &scalar);
    }
}
