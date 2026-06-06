//! `MarkGrid` — the candidates ("pencil marks"): for each cell, the [`Mark`] of
//! digits it may still take. The candidate atom, paired with a [`super::DigitGrid`]
//! inside a [`super::Board`].
//!
//! A `MarkGrid` is always *derived* from placements (`from_digits`) and then
//! maintained as digits are placed (`place`); it carries no information a
//! `DigitGrid` doesn't already determine, so it is never a source of truth on its
//! own. A filled cell's entry is `Mark::EMPTY`.

use super::{CELLS, CellIdx, Digit, DigitGrid, Mark, Marks, PEERS};

/// Per-cell naked candidate marks.
#[derive(Clone, PartialEq, Eq)]
pub struct MarkGrid([Mark; CELLS]);

impl MarkGrid {
    /// The marks of an empty board: every cell holds all nine candidates.
    pub const FULL: Self = MarkGrid([Mark::ALL; CELLS]);
}

/// The cell-major reference impl of the candidate-store contract.
impl Marks for MarkGrid {
    /// Derive the naked marks implied by a digit grid: every empty cell starts
    /// with all digits, then each placed digit is removed from its peers. Reads
    /// only the placements.
    fn from_digits(digits: &DigitGrid) -> Self {
        let mut m: [Mark; CELLS] =
            core::array::from_fn(|i| if digits.is_empty(i) { Mark::ALL } else { Mark::EMPTY });
        for i in 0..CELLS {
            if let Some(d) = digits.get(i) {
                for &peer in &PEERS[i] {
                    m[peer].remove(d);
                }
            }
        }
        MarkGrid(m)
    }

    /// Maintain the marks for placing digit `d` at cell `i`: cell `i` takes no
    /// further candidates, and `d` leaves every peer.
    fn place(&mut self, i: CellIdx, d: Digit) {
        self.0[i] = Mark::EMPTY;
        for &peer in &PEERS[i] {
            self.0[peer].remove(d);
        }
    }

    #[inline]
    fn get(&self, i: CellIdx) -> Mark {
        self.0[i]
    }
}

impl MarkGrid {
    /// Remove candidate `d` from cell `i` without placing — the technique layer's
    /// pruning primitive (locked candidates / subsets eliminate without deciding a
    /// cell). The cell-major counterpart of [`SearchState::forbid`](super::SearchState).
    #[inline]
    pub fn eliminate(&mut self, i: CellIdx, d: Digit) {
        self.0[i].remove(d);
    }
}
