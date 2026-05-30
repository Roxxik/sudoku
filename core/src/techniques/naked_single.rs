use smallvec::smallvec;

use crate::board::{Board, CELLS, iter_digits, popcount};
use crate::techniques::{Deduction, Step, TechniqueKind};

fn find_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    for i in 0..CELLS {
        if !board.is_empty(i) {
            continue;
        }
        let mask = board.candidates(i);
        if popcount(mask) == 1 {
            let d = iter_digits(mask).next().unwrap();
            let step = Step {
                technique: TechniqueKind::NakedSingle,
                deductions: smallvec![Deduction::Place { cell: i, digit: d }],
                focus_cells: smallvec![i],
                focus_house: None,
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

    #[test]
    fn detects_only_remaining_digit() {
        let mut b = Board::empty();
        for d in 1u8..=8 {
            b.eliminate(0, d);
        }
        let steps = find_all(&b);
        let step = steps.first().expect("should find naked single at cell 0");
        assert_eq!(step.deductions[..], [Deduction::Place { cell: 0, digit: 9 }]);
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

    #[test]
    fn find_first_returns_one() {
        let mut b = Board::empty();
        for i in 0..3 {
            for d in 1u8..=8 {
                b.eliminate(i, d);
            }
        }
        assert!(find_first(&b).is_some());
    }
}
