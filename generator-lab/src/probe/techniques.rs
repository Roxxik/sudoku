//! Composable solving techniques over the [`Marks`](crate::repr::Marks)
//! candidate-store contract.
//!
//! Each technique is written ONCE, generic over any `Marks` packing, using only
//! the trait's `get`/`place` plus the [`UNITS`](crate::repr::UNITS) geometry — so
//! swapping the representation (scalar `MarkGrid`/`Board`, banded single- or
//! dual-view) is a change of type parameter, never a re-implementation. This is the
//! *composable* layer: correct, packing-agnostic, and the kept default. If a
//! benchmark later shows the composable form isn't performant enough for the hot
//! path, a fused per-band fast path (the shape of bb's `band_update_rm`, which reads
//! a band's candidates once and drives several units from it) can plug in behind the
//! same surface — but the composable version stays the fallback.
//!
//! New code: the per-repr engines (`techniques.rs`, `bb`, `simt`) are untouched and
//! still drive the shipped generators.

use crate::repr::{CELLS, Digit, Marks, UNITS};

/// One sweep of **naked singles**: any cell left with exactly one candidate must
/// hold it — place it. Returns whether any placement was made (the caller loops to
/// a fixpoint). Like [`hidden_singles`], reads candidates fresh per cell, so a
/// placement made earlier in the sweep is seen by later cells. A filled cell has
/// an empty mark (`len() == 0`), so it is naturally skipped.
pub fn naked_singles<M: Marks>(marks: &mut M) -> bool {
    let mut changed = false;
    for cell in 0..CELLS {
        let m = marks.get(cell);
        if m.len() == 1 {
            marks.place(cell, m.iter().next().expect("exactly one candidate"));
            changed = true;
        }
    }
    changed
}

/// One sweep of **hidden singles**: for every unit and digit, if the digit has
/// exactly one candidate cell left in that unit, it is forced there — place it.
/// Returns whether any placement was made, so a caller can loop to a fixpoint and
/// interleave other techniques.
///
/// Candidates are read fresh per (unit, digit) through [`Marks::get`], so a
/// placement made earlier in the sweep is seen by later units — the same
/// intra-sweep cascade bb's band update gets by re-reading `live`. Hidden-single
/// placements are forced, so the fixpoint is independent of unit/digit order
/// (which is what lets two packings be compared at the fixpoint).
pub fn hidden_singles<M: Marks>(marks: &mut M) -> bool {
    let mut changed = false;
    for unit in &UNITS {
        for di in 0..9 {
            let d = Digit::from_index(di);
            // The lone candidate cell, or `None` once a second is seen (so a digit
            // already placed in the unit — zero candidate cells — is skipped).
            let mut only = None;
            for &cell in unit {
                if marks.get(cell).contains(d) {
                    if only.is_some() {
                        only = None;
                        break;
                    }
                    only = Some(cell);
                }
            }
            if let Some(cell) = only {
                marks.place(cell, d);
                changed = true;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::hidden_singles;
    use crate::repr::banded::DualSolverState;
    use crate::repr::{Board, CELLS, DigitGrid, MarkGrid, Marks};

    /// A classic puzzle solvable largely by singles, and its unique solution — so
    /// hidden singles genuinely fire and every placement can be checked.
    const PUZZLE: &str = "\
        53..7....\
        6..195...\
        .98....6.\
        8...6...3\
        4..8.3..1\
        7...2...6\
        .6....28.\
        ...419..5\
        ....8..79";
    const SOLUTION: &str = "\
        534678912\
        672195348\
        198342567\
        859761423\
        426853791\
        713924856\
        961537284\
        287419635\
        345286179";

    /// Drive hidden singles to a fixpoint on packing `M`, reporting whether any
    /// fired (to prove the test isn't vacuous).
    fn fixpoint<M: Marks>(grid: &DigitGrid) -> (M, bool) {
        let mut m = M::from_digits(grid);
        let mut fired = false;
        while hidden_singles(&mut m) {
            fired = true;
        }
        (m, fired)
    }

    /// The one generic `hidden_singles` must reach the identical candidate state on
    /// the cell-major scalar `MarkGrid` and the dual-view banded grid — the proof
    /// that the technique is genuinely packing-agnostic (the de-dup of bb's two
    /// `band_update_rm` copies), not just two parallel implementations.
    #[test]
    fn hidden_singles_agree_across_packings() {
        let grid = DigitGrid::parse(PUZZLE).unwrap();
        let (scalar, fired) = fixpoint::<MarkGrid>(&grid);
        let (banded, _) = fixpoint::<DualSolverState>(&grid);
        assert!(fired, "no hidden single fired — test is vacuous");
        for c in 0..CELLS {
            assert_eq!(scalar.get(c), banded.get(c), "cell {c}");
        }
    }

    /// Soundness: run on a `Board` (which tracks placements) and check every digit
    /// it places matches the known solution — hidden singles are forced, so a valid
    /// puzzle can never yield a wrong one.
    #[test]
    fn hidden_singles_place_correctly() {
        let grid = DigitGrid::parse(PUZZLE).unwrap();
        let solution = DigitGrid::parse(SOLUTION).unwrap();
        let (board, fired): (Board, bool) = fixpoint(&grid);
        assert!(fired, "no hidden single fired — test is vacuous");
        for c in 0..CELLS {
            if let Some(d) = board.cell(c) {
                assert_eq!(Some(d), solution.get(c), "cell {c}");
            }
        }
    }
}
