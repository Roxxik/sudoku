use crate::board::{Board, all_units, iter_digits, popcount};
use crate::techniques::{Deduction, HouseRef, Step, TechniqueKind};
use crate::util::for_each_combination;

pub fn find_pairs(board: &Board) -> Vec<Step> {
    find_subset(board, 2, TechniqueKind::NakedPair)
}

pub fn find_triples(board: &Board) -> Vec<Step> {
    find_subset(board, 3, TechniqueKind::NakedTriple)
}

pub fn find_quads(board: &Board) -> Vec<Step> {
    find_subset(board, 4, TechniqueKind::NakedQuad)
}

fn find_subset(board: &Board, size: usize, kind: TechniqueKind) -> Vec<Step> {
    let mut out = Vec::new();
    for (unit_kind, idx, unit) in all_units() {
        let candidate_cells: Vec<usize> = unit
            .iter()
            .copied()
            .filter(|&c| {
                if !board.is_empty(c) {
                    return false;
                }
                let n = popcount(board.candidates(c)) as usize;
                n >= 2 && n <= size
            })
            .collect();
        if candidate_cells.len() < size {
            continue;
        }
        for_each_combination(&candidate_cells, size, |combo| {
            let union: u16 = combo.iter().map(|&c| board.candidates(c)).fold(0, |a, b| a | b);
            if popcount(union) != size as u32 {
                return;
            }
            let mut eliminations = Vec::new();
            for &cell in unit {
                if combo.contains(&cell) || !board.is_empty(cell) {
                    continue;
                }
                let to_remove = board.candidates(cell) & union;
                for d in iter_digits(to_remove) {
                    eliminations.push(Deduction::Eliminate { cell, digit: d });
                }
            }
            if eliminations.is_empty() {
                return;
            }
            let house = HouseRef {
                kind: unit_kind,
                index: idx as u8,
            };
            out.push(Step {
                technique: kind,
                deductions: eliminations,
                focus_cells: combo.to_vec(),
                focus_house: Some(house),
            });
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_naked_pair_in_row() {
        let mut b = Board::empty();
        // Force cells 0 and 1 (row 0) to candidates {3, 7}
        for d in [1u8, 2, 4, 5, 6, 8, 9] {
            b.eliminate(0, d);
            b.eliminate(1, d);
        }
        let steps = find_pairs(&b);
        let row_step = steps
            .iter()
            .find(|s| {
                matches!(
                    s.focus_house.as_ref().map(|h| h.kind),
                    Some(crate::board::UnitKind::Row)
                )
            })
            .expect("expected a row-based naked pair");
        // Eliminations should remove 3 and 7 from cells 2..=8 in row 0
        let count = row_step.deductions.len();
        assert_eq!(count, 14); // 7 cells x 2 digits
    }

    #[test]
    fn detects_naked_triple_in_box() {
        let mut b = Board::empty();
        // Three cells in box 0 with combined candidates exactly {3,5,7}
        // Cell 0: {3, 5}
        for d in [1u8, 2, 4, 6, 7, 8, 9] {
            b.eliminate(0, d);
        }
        // Cell 1: {3, 7}
        for d in [1u8, 2, 4, 5, 6, 8, 9] {
            b.eliminate(1, d);
        }
        // Cell 2: {5, 7}
        for d in [1u8, 2, 3, 4, 6, 8, 9] {
            b.eliminate(2, d);
        }
        let steps = find_triples(&b);
        assert!(!steps.is_empty(), "expected at least one naked triple");
    }
}
