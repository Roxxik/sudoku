use crate::board::{Board, CELLS, iter_digits, popcount};
use crate::techniques::{Deduction, Step, TechniqueKind};

pub fn find_all(board: &Board) -> Vec<Step> {
    let mut out = Vec::new();
    for i in 0..CELLS {
        if !board.is_empty(i) {
            continue;
        }
        let mask = board.candidates(i);
        if popcount(mask) == 1 {
            let d = iter_digits(mask).next().unwrap();
            out.push(Step {
                technique: TechniqueKind::NakedSingle,
                deductions: vec![Deduction::Place { cell: i, digit: d }],
                focus_cells: vec![i],
                focus_house: None,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_only_remaining_digit() {
        let mut b = Board::empty();
        for d in 1u8..=8 {
            b.eliminate(0, d);
        }
        let steps = find_all(&b);
        let step = steps.first().expect("should find naked single at cell 0");
        assert_eq!(step.deductions, vec![Deduction::Place { cell: 0, digit: 9 }]);
    }

    #[test]
    fn find_all_returns_every_naked_single() {
        let mut b = Board::empty();
        for i in 0..3 {
            for d in 1u8..=8 {
                b.eliminate(i, d);
            }
        }
        let steps = find_all(&b);
        assert_eq!(steps.len(), 3);
    }
}
