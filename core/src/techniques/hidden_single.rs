use smallvec::smallvec;

use crate::board::{Board, all_units, digit_to_bit};
use crate::techniques::{Deduction, HouseRef, Step, TechniqueKind};

fn find_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    for (kind, idx, unit) in all_units() {
        // Single pass over the unit: `once` is the set of digits appearing as a
        // candidate of at least one empty cell, `twice` the set appearing in two
        // or more. A hidden single is a digit that occurs in exactly one empty
        // cell, i.e. `once & !twice`. (A digit already placed in the unit can't
        // be a candidate of any empty peer, so it never enters `once`; mask it
        // out anyway for safety.) This replaces the old 9-digit x 9-cell sweep
        // with one 9-cell sweep, while emitting the identical steps in the same
        // order: ascending unit, then ascending digit, then the unique cell.
        let mut placed = 0u16;
        let mut once = 0u16;
        let mut twice = 0u16;
        for &cell in unit {
            let v = board.cell(cell);
            if v != 0 {
                placed |= digit_to_bit(v);
            } else {
                let m = board.candidates(cell);
                twice |= once & m;
                once |= m;
            }
        }
        let mut singles = once & !twice & !placed;
        while singles != 0 {
            let bit = singles & singles.wrapping_neg(); // lowest set candidate bit
            singles &= singles - 1;
            let d = bit.trailing_zeros() as u8 + 1;
            // Exactly one empty cell in the unit carries this bit; find it.
            let mut location = 0;
            for &cell in unit {
                if board.is_empty(cell) && board.candidates(cell) & bit != 0 {
                    location = cell;
                    break;
                }
            }
            let step = Step {
                technique: TechniqueKind::HiddenSingle,
                deductions: smallvec![Deduction::Place {
                    cell: location,
                    digit: d,
                }],
                focus_cells: smallvec![location],
                focus_house: Some(HouseRef {
                    kind,
                    index: idx as u8,
                }),
            };
            if !emit(step) {
                return;
            }
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;

    #[test]
    fn detects_hidden_single_in_row() {
        let mut b = Board::empty();
        for c in 1..9 {
            b.eliminate(c, 7);
        }
        let steps = find_all(&b);
        let step = steps.first().expect("should find hidden single for 7 in row 0");
        assert_eq!(step.deductions[..], [Deduction::Place { cell: 0, digit: 7 }]);
    }
}
