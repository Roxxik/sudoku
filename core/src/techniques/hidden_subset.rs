use crate::board::{Board, all_units, digit_to_bit, iter_digits};
use crate::techniques::{Deduction, HouseRef, Step, TechniqueKind};
use crate::util::for_each_combination;

pub fn find_pairs(board: &Board) -> Vec<Step> {
    find_subset(board, 2, TechniqueKind::HiddenPair)
}

pub fn find_triples(board: &Board) -> Vec<Step> {
    find_subset(board, 3, TechniqueKind::HiddenTriple)
}

pub fn find_quads(board: &Board) -> Vec<Step> {
    find_subset(board, 4, TechniqueKind::HiddenQuad)
}

fn find_subset(board: &Board, size: usize, kind: TechniqueKind) -> Vec<Step> {
    let mut out = Vec::new();
    for (unit_kind, idx, unit) in all_units() {
        let mut positions: [u16; 10] = [0; 10];
        let mut digits_in_play: Vec<u8> = Vec::new();
        let mut already_placed = [false; 10];
        for d in 1u8..=9 {
            let bit = digit_to_bit(d);
            let mut placed = false;
            let mut pos: u16 = 0;
            for (i, &cell) in unit.iter().enumerate() {
                if board.cell(cell) == d {
                    placed = true;
                    break;
                }
                if board.is_empty(cell) && board.candidates(cell) & bit != 0 {
                    pos |= 1 << i;
                }
            }
            already_placed[d as usize] = placed;
            if !placed {
                let n = pos.count_ones() as usize;
                if n >= 2 && n <= size {
                    positions[d as usize] = pos;
                    digits_in_play.push(d);
                }
            }
        }
        if digits_in_play.len() < size {
            continue;
        }

        for_each_combination(&digits_in_play, size, |combo| {
            let union: u16 = combo
                .iter()
                .map(|&d| positions[d as usize])
                .fold(0, |a, b| a | b);
            if union.count_ones() as usize != size {
                return;
            }
            let keep_mask: u16 = combo.iter().map(|&d| digit_to_bit(d)).fold(0, |a, b| a | b);
            let mut eliminations = Vec::new();
            let mut cells_in_subset: Vec<usize> = Vec::new();
            for i in 0..9 {
                if union & (1 << i) == 0 {
                    continue;
                }
                let cell = unit[i];
                cells_in_subset.push(cell);
                let to_remove = board.candidates(cell) & !keep_mask;
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
                focus_cells: cells_in_subset,
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
    fn detects_hidden_pair_in_row() {
        let mut b = Board::empty();
        // In row 0, digits 3 and 7 should only appear at cells 0 and 1.
        // Remove 3 and 7 from cells 2..=8 of row 0.
        for c in 2..9 {
            b.eliminate(c, 3);
            b.eliminate(c, 7);
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
            .expect("expected hidden pair in row 0");
        // Should eliminate the 7 other candidates from each of cells 0 and 1 → 14 eliminations
        assert_eq!(row_step.deductions.len(), 14);
    }
}
