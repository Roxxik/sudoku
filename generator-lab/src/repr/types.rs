//! Simple board value types with no behaviour of their own: the [`Digit`]
//! primitive, and the intent-tagged grids [`Puzzle`] / [`Solution`].
//!
//! `Puzzle` and `Solution` are both just a [`DigitGrid`]; the newtype says which
//! one we mean so signatures can speak the domain (`fill` yields a `Solution`,
//! stripping a `Solution` yields a `Puzzle`). The invariants (a `Solution` is
//! complete; a `Puzzle` is sparse and uniquely solvable) are not enforced here.

use std::ops::Deref;
use super::DigitGrid;

/// A cell index, `0..81` (row-major).
pub type CellIdx = usize;

/// A fully-filled grid: a sudoku solution.
#[derive(Clone, PartialEq, Eq)]
pub struct Solution(pub DigitGrid);

/// A grid of clues: a sudoku puzzle.
#[derive(Clone, PartialEq, Eq)]
pub struct Puzzle(pub DigitGrid);

impl Deref for Solution {
    type Target = DigitGrid;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Deref for Puzzle {
    type Target = DigitGrid;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
