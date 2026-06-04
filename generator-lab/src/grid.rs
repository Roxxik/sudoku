//! Minimal grid primitives, copied verbatim from `core::board` (trimmed to what
//! the solver lab needs). Kept self-contained so the lab has zero dependency on
//! `core` and each solver can be benchmarked in isolation.

pub const N: usize = 9;
pub const CELLS: usize = 81;
pub const ALL_DIGITS: u16 = 0x1FF;

pub type Digit = u8;
pub type CellIdx = usize;
/// A 9-bit set of digits: bit `d-1` set iff digit `d` is present (e.g. a cell's
/// candidate mask). Distinct from [`crate::technique_kinds::KindMask`], the set of
/// technique *kinds*.
pub type DigitMask = u16;

#[inline(always)]
pub const fn digit_to_bit(d: Digit) -> DigitMask {
    1u16 << (d as u16 - 1)
}

#[inline(always)]
pub const fn row_of(i: CellIdx) -> usize {
    i / 9
}

#[inline(always)]
pub const fn col_of(i: CellIdx) -> usize {
    i % 9
}

#[inline(always)]
pub const fn box_of(i: CellIdx) -> usize {
    (i / 9 / 3) * 3 + (i % 9) / 3
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    Row,
    Col,
    Box,
}

/// Iterate the set digits in `mask`, lowest bit first.
#[inline]
pub fn iter_digits(mask: DigitMask) -> DigitIter {
    DigitIter {
        mask: mask & ALL_DIGITS,
    }
}

pub struct DigitIter {
    mask: DigitMask,
}

impl Iterator for DigitIter {
    type Item = Digit;

    #[inline]
    fn next(&mut self) -> Option<Digit> {
        if self.mask == 0 {
            return None;
        }
        let bit = self.mask & self.mask.wrapping_neg();
        let d = bit.trailing_zeros() as Digit + 1;
        self.mask &= self.mask - 1;
        Some(d)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.mask.count_ones() as usize;
        (n, Some(n))
    }
}

#[inline(always)]
pub fn popcount(mask: DigitMask) -> u32 {
    (mask & ALL_DIGITS).count_ones()
}

/// A 9x9 grid plus per-cell *naked* candidate masks, maintained incrementally on
/// `place`/`clear_naked` — same shape as `core::board::Board`.
#[derive(Clone, PartialEq, Eq)]
pub struct Board {
    cells: [Digit; CELLS],
    candidates: [DigitMask; CELLS],
}

impl Board {
    pub fn empty() -> Self {
        Self {
            cells: [0; CELLS],
            candidates: [ALL_DIGITS; CELLS],
        }
    }

    /// Build a *complete* solution board directly from its 81 digits. Every cell
    /// is filled, so the naked candidate masks are all zero — set them so
    /// without the O(81×20) per-`place` peer maintenance. Used by
    /// `random_full_grid`, whose bitboard fill produces the digits directly.
    pub fn from_solved_cells(cells: [Digit; CELLS]) -> Self {
        debug_assert!(
            cells.iter().all(|&d| d != 0),
            "from_solved_cells on an incomplete grid"
        );
        Self {
            cells,
            candidates: [0; CELLS],
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let chars: Vec<char> = s
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '|' && *c != '-' && *c != '+')
            .collect();
        if chars.len() != CELLS {
            return None;
        }
        let mut board = Self::empty();
        for (i, &c) in chars.iter().enumerate() {
            let d = match c {
                '.' | '0' => continue,
                '1'..='9' => c.to_digit(10).unwrap() as Digit,
                _ => return None,
            };
            if board.candidates[i] & digit_to_bit(d) == 0 {
                return None;
            }
            board.place(i, d);
        }
        Some(board)
    }

    #[inline]
    pub fn cell(&self, i: CellIdx) -> Digit {
        self.cells[i]
    }

    /// The 81 cell digits (0 = empty) as a flat array — for fingerprint folding.
    #[inline]
    pub fn cells(&self) -> &[Digit; CELLS] {
        &self.cells
    }

    #[inline]
    pub fn candidates(&self, i: CellIdx) -> DigitMask {
        self.candidates[i]
    }

    #[inline]
    pub fn is_empty(&self, i: CellIdx) -> bool {
        self.cells[i] == 0
    }

    pub fn place(&mut self, i: CellIdx, d: Digit) {
        debug_assert!(self.cells[i] == 0, "placing on a filled cell");
        debug_assert!(
            self.candidates[i] & digit_to_bit(d) != 0,
            "placing impossible digit"
        );
        self.cells[i] = d;
        self.candidates[i] = 0;
        let bit = digit_to_bit(d);
        for &peer in &PEERS[i] {
            self.candidates[peer] &= !bit;
        }
    }

    /// Single-cell clear specialized to boards whose candidate masks are the
    /// *naked* candidates (placements only). Removing digit `d` at cell `i`
    /// changes only cell `i`'s candidates and, for `d`, those of `i`'s peers;
    /// nothing else moves. Produces byte-identical naked candidates to a full
    /// `recompute_candidates` (the debug assertion cross-checks).
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn clear_naked(&mut self, i: CellIdx) {
        let d = self.cells[i];
        if d == 0 {
            return;
        }
        self.cells[i] = 0;
        let mut c = ALL_DIGITS;
        for &peer in &PEERS[i] {
            let pd = self.cells[peer];
            if pd != 0 {
                c &= !digit_to_bit(pd);
            }
        }
        self.candidates[i] = c;
        let bit = digit_to_bit(d);
        for &p in &PEERS[i] {
            if self.cells[p] != 0 {
                continue;
            }
            let mut blocked = false;
            for &pp in &PEERS[p] {
                if self.cells[pp] == d {
                    blocked = true;
                    break;
                }
            }
            if !blocked {
                self.candidates[p] |= bit;
            }
        }
        debug_assert!(
            {
                let mut replay = self.clone();
                replay.recompute_candidates();
                replay.candidates == self.candidates
            },
            "clear_naked diverged from recompute_candidates at cell {}",
            i
        );
    }

    /// Clear digit `d` from cell `i`'s candidate mask. Returns whether it
    /// changed anything. Used by the technique solver (which applies arbitrary
    /// per-cell eliminations on a working clone). Mirrors `core::board::eliminate`.
    pub fn eliminate(&mut self, i: CellIdx, d: Digit) -> bool {
        let bit = digit_to_bit(d);
        if self.candidates[i] & bit == 0 {
            return false;
        }
        self.candidates[i] &= !bit;
        true
    }

    pub fn recompute_candidates(&mut self) {
        for i in 0..CELLS {
            self.candidates[i] = if self.cells[i] != 0 { 0 } else { ALL_DIGITS };
        }
        for i in 0..CELLS {
            let d = self.cells[i];
            if d == 0 {
                continue;
            }
            let bit = digit_to_bit(d);
            for &peer in &PEERS[i] {
                self.candidates[peer] &= !bit;
            }
        }
    }

    pub fn is_solved(&self) -> bool {
        self.cells.iter().all(|&c| c != 0)
    }

    pub fn givens(&self) -> usize {
        self.cells.iter().filter(|&&c| c != 0).count()
    }

    pub fn to_line(&self) -> String {
        self.cells
            .iter()
            .map(|&c| if c == 0 { '.' } else { (b'0' + c) as char })
            .collect()
    }
}

pub const ROW_UNITS: [[CellIdx; 9]; 9] = build_row_units();
pub const COL_UNITS: [[CellIdx; 9]; 9] = build_col_units();
pub const BOX_UNITS: [[CellIdx; 9]; 9] = build_box_units();
pub const PEERS: [[CellIdx; 20]; 81] = build_peers();

/// All 27 units (9 rows, 9 cols, 9 boxes), yielded as `(kind, index, cells)`.
/// Order matters: technique searches scan rows, then cols, then boxes.
pub fn all_units() -> impl Iterator<Item = (UnitKind, usize, &'static [CellIdx; 9])> {
    ROW_UNITS
        .iter()
        .enumerate()
        .map(|(i, u)| (UnitKind::Row, i, u))
        .chain(COL_UNITS.iter().enumerate().map(|(i, u)| (UnitKind::Col, i, u)))
        .chain(BOX_UNITS.iter().enumerate().map(|(i, u)| (UnitKind::Box, i, u)))
}

const fn build_row_units() -> [[CellIdx; 9]; 9] {
    let mut u = [[0usize; 9]; 9];
    let mut r = 0;
    while r < 9 {
        let mut c = 0;
        while c < 9 {
            u[r][c] = r * 9 + c;
            c += 1;
        }
        r += 1;
    }
    u
}

const fn build_col_units() -> [[CellIdx; 9]; 9] {
    let mut u = [[0usize; 9]; 9];
    let mut c = 0;
    while c < 9 {
        let mut r = 0;
        while r < 9 {
            u[c][r] = r * 9 + c;
            r += 1;
        }
        c += 1;
    }
    u
}

const fn build_box_units() -> [[CellIdx; 9]; 9] {
    let mut u = [[0usize; 9]; 9];
    let mut b = 0;
    while b < 9 {
        let br = (b / 3) * 3;
        let bc = (b % 3) * 3;
        let mut k = 0;
        let mut dr = 0;
        while dr < 3 {
            let mut dc = 0;
            while dc < 3 {
                u[b][k] = (br + dr) * 9 + (bc + dc);
                k += 1;
                dc += 1;
            }
            dr += 1;
        }
        b += 1;
    }
    u
}

const fn build_peers() -> [[CellIdx; 20]; 81] {
    let mut peers = [[0usize; 20]; 81];
    let mut i = 0;
    while i < 81 {
        let row = i / 9;
        let col = i % 9;
        let br = (row / 3) * 3;
        let bc = (col / 3) * 3;
        let mut k = 0;
        let mut j = 0;
        while j < 81 {
            if j != i {
                let jr = j / 9;
                let jc = j % 9;
                let same_row = jr == row;
                let same_col = jc == col;
                let same_box = jr >= br && jr < br + 3 && jc >= bc && jc < bc + 3;
                if same_row || same_col || same_box {
                    peers[i][k] = j;
                    k += 1;
                }
            }
            j += 1;
        }
        i += 1;
    }
    peers
}
