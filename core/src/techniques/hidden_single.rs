use crate::board::{Board, all_units, digit_to_bit};
use crate::techniques::{Deduction, HouseRef, Step, TechniqueKind};

fn find_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    for (kind, idx, unit) in all_units() {
        for d in 1u8..=9 {
            let bit = digit_to_bit(d);
            let mut count = 0;
            let mut location: usize = 0;
            let mut already_placed = false;
            for &cell in unit {
                if board.cell(cell) == d {
                    already_placed = true;
                    break;
                }
                if board.is_empty(cell) && board.candidates(cell) & bit != 0 {
                    count += 1;
                    location = cell;
                }
            }
            if !already_placed && count == 1 {
                let step = Step {
                    technique: TechniqueKind::HiddenSingle,
                    deductions: vec![Deduction::Place {
                        cell: location,
                        digit: d,
                    }],
                    focus_cells: vec![location],
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
        assert_eq!(step.deductions, vec![Deduction::Place { cell: 0, digit: 7 }]);
    }
}
