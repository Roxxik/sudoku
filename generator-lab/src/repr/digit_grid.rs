//! `DigitGrid` — the placements: one [`Digit`] per cell, or empty (`None`).
//!
//! A sudoku puzzle and a solution are both just a grid of placements; they differ
//! only in how many cells are filled, not in type (see [`super::Puzzle`] /
//! [`super::Solution`]). A cell is `Option<Digit>` — `None` is empty — so there is
//! no "is `0` a digit?" ambiguity, and the byte layout is unchanged (the
//! `NonZeroU8` niche makes `None` == 0). `DigitGrid` carries no candidate state,
//! so its only invariant is "these are placements" — the candidate-maintaining
//! work lives one layer up in [`super::Board`].

use super::{CELLS, CellIdx, Digit};

/// 81 cells, row-major, each holding a [`Digit`] or empty (`None`).
#[derive(Clone, PartialEq, Eq)]
pub struct DigitGrid([Option<Digit>; CELLS]);

impl DigitGrid {
    pub const EMPTY: Self = DigitGrid([None; CELLS]);

    pub fn from_array(cells: [Option<Digit>; CELLS]) -> Self {
        DigitGrid(cells)
    }

    /// Parse 81 chars (`.`/`0` = empty, `1`-`9` = a digit); whitespace and the
    /// `|-+` box-border glyphs are ignored. `None` on a bad length or an
    /// out-of-range char. This only *reads* placements — it does NOT check the
    /// result is a legal sudoku (no peer-conflict test); that validation belongs
    /// to whoever cares, not to a bare digit grid.
    pub fn parse(s: &str) -> Option<Self> {
        let chars: Vec<char> = s
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '|' && *c != '-' && *c != '+')
            .collect();
        if chars.len() != CELLS {
            return None;
        }
        let mut g = [None; CELLS];
        for (i, &c) in chars.iter().enumerate() {
            g[i] = match c {
                '.' | '0' => None,
                '1'..='9' => Digit::new(c as u8 - b'0'),
                _ => return None,
            };
        }
        Some(DigitGrid(g))
    }

    #[inline]
    pub fn get(&self, i: CellIdx) -> Option<Digit> {
        self.0[i]
    }

    #[inline]
    pub fn set(&mut self, i: CellIdx, d: Digit) {
        self.0[i] = Some(d);
    }

    #[inline]
    pub fn clear(&mut self, i: CellIdx) {
        self.0[i] = None;
    }

    /// The 81 cells as a flat array (e.g. for fingerprint folding).
    #[inline]
    pub fn cells(&self) -> &[Option<Digit>; CELLS] {
        &self.0
    }

    /// The 81 cells as raw bytes — `1..=9` for a digit, `0` for empty (the line/byte form;
    /// [`cells`](Self::cells) keeps the `Option`s). Used for fingerprint folding.
    #[inline]
    pub fn cell_bytes(&self) -> [u8; CELLS] {
        core::array::from_fn(|i| self.0[i].map_or(0, |d| d.get()))
    }

    #[inline]
    pub fn is_empty(&self, i: CellIdx) -> bool {
        self.0[i].is_none()
    }

    /// Every cell filled.
    pub fn is_complete(&self) -> bool {
        self.0.iter().all(|c| c.is_some())
    }

    /// Number of cells holding a digit. Note this can't distinguish an initial
    /// clue from a placed digit — "givens" is only meaningful once a grid is
    /// known to be a `Puzzle`.
    pub fn digit_count(&self) -> usize {
        self.0.iter().filter(|c| c.is_some()).count()
    }

    pub fn to_line(&self) -> String {
        self.0
            .iter()
            .map(|c| match c {
                Some(d) => (b'0' + d.get()) as char,
                None => '.',
            })
            .collect()
    }
}
