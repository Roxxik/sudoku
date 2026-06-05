//! `RowBandedDigitGrid` — the placements in the row-major banding: for each digit,
//! the set of cells holding it. The banded analogue of [`super::super::DigitGrid`],
//! stored *by digit* (one [`Bands`] per digit) rather than by cell.
//!
//! Storing placements by digit is what the candidate bands can't cheaply answer
//! on their own — *which* surviving peer holds *which* digit — so this grid is
//! the missing fact a banded re-open needs: with it, the cells a clue still
//! forbids are read off band masks instead of a per-peer scan of a scalar grid.
//! Held alongside, not inside, the candidate grid, so it never bloats a search's
//! per-branch clone.

use super::banding::RowMajor;
use super::bands::Bands;
use crate::repr::{CELLS, CellIdx, Digit, DigitGrid, PerDigit};

/// Per-digit clue positions, row-major banded: `digit_cells[d]` is the set of
/// cells holding digit `d`.
#[derive(Clone, PartialEq, Eq)]
pub struct RowBandedDigitGrid {
    digit_cells: PerDigit<Bands<RowMajor>>,
}

impl RowBandedDigitGrid {
    /// Band the placements of a [`DigitGrid`]: drop each filled cell into its
    /// digit's set, leave the empties out.
    pub fn from_digits(grid: &DigitGrid) -> Self {
        let mut digit_cells = PerDigit::new([Bands::<RowMajor>::EMPTY; 9]);
        for cell in 0..CELLS {
            if let Some(d) = grid.get(cell) {
                digit_cells[d].insert(cell);
            }
        }
        RowBandedDigitGrid { digit_cells }
    }

    /// Record cell `cell` as now holding digit `d`.
    pub fn place(&mut self, cell: CellIdx, d: Digit) {
        self.digit_cells[d].insert(cell);
    }

    /// Drop cell `cell` (which held digit `d`) from the placements.
    pub fn clear(&mut self, cell: CellIdx, d: Digit) {
        self.digit_cells[d].remove(cell);
    }
}
