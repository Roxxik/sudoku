//! `Board` — the scalar working representation: a [`DigitGrid`] plus, for each
//! empty cell, its *marks*: the naked candidates (the digits no filled peer rules
//! out).
//!
//! The two halves are kept consistent at every mutation; that maintenance is the
//! whole reason `Board` exists as a type rather than two loose grids. Strategy
//! that only *reads* this state to solve — the technique engine, unit scans,
//! arbitrary mark eliminations — lives a layer up, not here.

use super::{CellIdx, Digit, DigitGrid, Mark, MarkGrid, Marks, Occupancy};

/// The mid-solve working state: digits + per-cell naked marks, kept consistent at
/// every mutation. A complete solution or a bare clue grid is a [`DigitGrid`] (no
/// live marks), not a `Board`.
#[derive(Clone, PartialEq, Eq)]
pub struct Board {
    digits: DigitGrid,
    marks: MarkGrid,
}

impl Board {
    pub const EMPTY: Self = Self {
        digits: DigitGrid::EMPTY,
        marks: MarkGrid::FULL,
    };

    /// Seed a working board from a clue grid: take its digits and derive every
    /// cell's naked marks in one bulk pass ([`MarkGrid::from_digits`]). The
    /// constructor counterpart to the incremental `place`.
    pub fn from_digits(digits: &DigitGrid) -> Self {
        Self {
            marks: MarkGrid::from_digits(digits),
            digits: digits.clone(),
        }
    }

    #[inline]
    pub fn digits(&self) -> &DigitGrid {
        &self.digits
    }

    #[inline]
    pub fn cell(&self, i: CellIdx) -> Option<Digit> {
        self.digits.get(i)
    }

    /// The naked marks (candidate set) of cell `i`.
    #[inline]
    pub fn marks(&self, i: CellIdx) -> Mark {
        self.marks.get(i)
    }

    pub fn is_complete(&self) -> bool {
        self.digits.is_complete()
    }

    pub fn digit_count(&self) -> usize {
        self.digits.digit_count()
    }

    pub fn place(&mut self, i: CellIdx, d: Digit) {
        debug_assert!(self.digits.is_empty(i), "placing on a filled cell");
        debug_assert!(
            self.marks.get(i).contains(d),
            "placing impossible digit"
        );
        self.digits.set(i, d);
        self.marks.place(i, d);
    }
}

/// `Board` is a candidate store too — the scalar reference a technique can run on
/// while also tracking placements (its `DigitGrid`), so a deduction's result can be
/// checked against a known solution, not just against another packing. The
/// invariant is the stronger one `Board` already keeps: digits and marks stay
/// consistent at every `place`.
impl Marks for Board {
    fn from_digits(grid: &DigitGrid) -> Self {
        Board::from_digits(grid)
    }
    fn place(&mut self, cell: CellIdx, d: Digit) {
        self.place(cell, d);
    }
    fn get(&self, cell: CellIdx) -> Mark {
        self.marks(cell)
    }
}

impl Occupancy for Board {
    #[inline]
    fn is_empty(&self, i: CellIdx) -> bool {
        self.digits.is_empty(i)
    }
}
