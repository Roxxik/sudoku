//! Phistomefel's Ring — a set-equality technique.
//!
//! The 16 cells forming the four 2x2 blocks in the grid's corners (set A) hold
//! exactly the same multiset of digits as the 16 cells forming the ring around
//! the central box (set B). The ring is the 5x5 square centred on the grid
//! (rows/cols 3..=7) with the central box (rows/cols 4..=6) removed.
//!
//! Proof sketch: the four outer rows (1,2,8,9) together with the four outer
//! columns (1,2,8,9) cover, by inclusion-exclusion, set A twice plus the band
//! and stack cells once; subtracting the four corner boxes and the outer ring
//! of digit-complete houses leaves multiset(A) = multiset(B). The identity holds
//! for any valid solution, independent of the givens.
//!
//! ## Deductions
//!
//! For every digit `d`, the solution count of `d` in A equals its count in B.
//! With `lo_R(d)` = cells of R already filled with `d` (a lower bound on the
//! count) and `hi_R(d)` = those plus empty cells of R still admitting `d` (an
//! upper bound), `lo_A <= count_A == count_B <= hi_B` and the symmetric chain
//! hold. So whenever one set's lower bound meets the other set's upper bound the
//! count is pinned at that value, which forces:
//!
//! - **loose set** (the one at its *lower* bound): no empty cell may take `d` —
//!   eliminate `d` there.
//! - **tight set** (the one at its *upper* bound): every empty cell that still
//!   admits `d` must take it — strip the cell's *other* candidates, setting up a
//!   naked single to place `d` (this crate's convention: only singles place).
//!
//! Both pin directions are emitted per digit, so the technique surfaces every
//! elimination the identity licenses on the current candidate state.

use crate::board::{Board, CellIdx, digit_to_bit, iter_digits};
use crate::techniques::{Deduction, Step, TechniqueKind};

/// The four corner 2x2 blocks (set A).
const CORNERS: [CellIdx; 16] = [
    0, 1, 9, 10, // top-left
    7, 8, 16, 17, // top-right
    63, 64, 72, 73, // bottom-left
    70, 71, 79, 80, // bottom-right
];

/// The ring around the central box (set B): rows/cols 3..=7 minus the centre box.
const RING: [CellIdx; 16] = [
    20, 21, 22, 23, 24, // row 3, cols 3..=7
    56, 57, 58, 59, 60, // row 7, cols 3..=7
    29, 33, // row 4, cols 3 & 7
    38, 42, // row 5, cols 3 & 7
    47, 51, // row 6, cols 3 & 7
];

pub fn find_all(board: &Board) -> Vec<Step> {
    let mut out = Vec::new();
    find_each(board, |s| {
        out.push(s);
        true
    });
    out
}

pub fn find_first(board: &Board) -> Option<Step> {
    let mut found = None;
    find_each(board, |s| {
        found = Some(s);
        false
    });
    found
}

/// Per-set occurrence bounds for a single digit.
struct Bounds {
    /// Cells already filled with the digit (the count's lower bound).
    lo: usize,
    /// Empty cells still admitting the digit.
    candidates: Vec<CellIdx>,
}

impl Bounds {
    /// Upper bound on the digit's count in this set.
    fn hi(&self) -> usize {
        self.lo + self.candidates.len()
    }
}

fn bounds_for(board: &Board, set: &[CellIdx], d: u8) -> Bounds {
    let bit = digit_to_bit(d);
    let mut lo = 0;
    let mut candidates = Vec::new();
    for &cell in set {
        if board.cell(cell) == d {
            lo += 1;
        } else if board.is_empty(cell) && board.candidates(cell) & bit != 0 {
            candidates.push(cell);
        }
    }
    Bounds { lo, candidates }
}

fn find_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    for d in 1u8..=9 {
        let a = bounds_for(board, &CORNERS, d);
        let b = bounds_for(board, &RING, d);

        // Pin direction 1: A at its lower bound, B at its upper bound.
        if a.lo == b.hi() {
            if let Some(step) = pin_step(board, d, &a, &b) {
                if !emit(step) {
                    return;
                }
            }
        }
        // Pin direction 2: B at its lower bound, A at its upper bound.
        if b.lo == a.hi() {
            if let Some(step) = pin_step(board, d, &b, &a) {
                if !emit(step) {
                    return;
                }
            }
        }
    }
}

/// Build the eliminations for a pinned count: `loose` is the set held at its
/// lower bound (digit `d` ruled out of its empty cells), `tight` the set held at
/// its upper bound (every empty candidate cell forced to `d`).
fn pin_step(board: &Board, d: u8, loose: &Bounds, tight: &Bounds) -> Option<Step> {
    let bit = digit_to_bit(d);
    let mut deductions = Vec::new();
    let mut focus_cells = Vec::new();

    // Loose set: no empty cell may take d.
    for &cell in &loose.candidates {
        deductions.push(Deduction::Eliminate { cell, digit: d });
        focus_cells.push(cell);
    }
    // Tight set: every empty candidate cell must take d; strip its other digits.
    for &cell in &tight.candidates {
        let others = board.candidates(cell) & !bit;
        if others == 0 {
            continue; // already a naked d; nothing to remove.
        }
        for other in iter_digits(others) {
            deductions.push(Deduction::Eliminate { cell, digit: other });
        }
        focus_cells.push(cell);
    }

    if deductions.is_empty() {
        return None;
    }
    Some(Step {
        technique: TechniqueKind::PhistomefelRing,
        deductions: deductions.into(),
        focus_cells: focus_cells.into(),
        focus_house: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::row_of;

    /// The geometry must satisfy Phistomefel's theorem: in any valid full
    /// solution, set A and set B hold the same multiset of digits.
    #[test]
    fn theorem_holds_on_a_solved_grid() {
        let solved = "534678912672195348198342567859761423426853791713924856961537284287419635345286179";
        let board = Board::parse(solved).unwrap();
        let mut count_a = [0usize; 10];
        let mut count_b = [0usize; 10];
        for &c in &CORNERS {
            count_a[board.cell(c) as usize] += 1;
        }
        for &c in &RING {
            count_b[board.cell(c) as usize] += 1;
        }
        assert_eq!(count_a, count_b, "Phistomefel multiset identity violated");
    }

    #[test]
    fn sets_are_disjoint_and_sized() {
        let mut seen = [false; 81];
        for &c in CORNERS.iter().chain(RING.iter()) {
            assert!(!seen[c], "cell {} appears in both/twice", c);
            seen[c] = true;
        }
        assert_eq!(CORNERS.len(), 16);
        assert_eq!(RING.len(), 16);
    }

    #[test]
    fn pins_eliminate_on_a_constructed_board() {
        // Empty the four corner 2x2 blocks of every candidate for digit 1
        // except keep nothing there, so count_A(1) is pinned to 0 (lo_A = 0,
        // and we drive hi_A to 0). Then count_B(1) must also be 0: digit 1 is
        // eliminated from every ring cell that still admits it.
        let mut b = Board::empty();
        for &c in &CORNERS {
            b.eliminate(c, 1);
        }
        let steps = find_all(&b);
        let step = steps
            .iter()
            .find(|s| {
                s.deductions
                    .iter()
                    .all(|d| matches!(d, Deduction::Eliminate { digit: 1, .. }))
            })
            .expect("expected digit-1 eliminations across the ring");
        // Every conclusion removes 1 from a ring cell.
        for d in &step.deductions {
            match d {
                Deduction::Eliminate { cell, digit: 1 } => {
                    assert!(RING.contains(cell), "elimination outside ring at {}", cell);
                    let _ = row_of(*cell);
                }
                other => panic!("unexpected deduction {:?}", other),
            }
        }
    }
}
