use crate::board::{
    BOX_UNITS, Board, COL_UNITS, ROW_UNITS, UnitKind, box_of, col_of, digit_to_bit, row_of,
};
use crate::techniques::{Deduction, HouseRef, Step, TechniqueKind};

pub fn find_pointing(board: &Board) -> Vec<Step> {
    let mut out = Vec::new();
    find_pointing_each(board, |s| {
        out.push(s);
        true
    });
    out
}

pub fn find_first_pointing(board: &Board) -> Option<Step> {
    let mut found = None;
    find_pointing_each(board, |s| {
        found = Some(s);
        false
    });
    found
}

fn find_pointing_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    for box_idx in 0..9usize {
        let unit = &BOX_UNITS[box_idx];
        for d in 1u8..=9 {
            let bit = digit_to_bit(d);
            let mut placed = false;
            let mut positions: Vec<usize> = Vec::new();
            for &cell in unit {
                if board.cell(cell) == d {
                    placed = true;
                    break;
                }
                if board.is_empty(cell) && board.candidates(cell) & bit != 0 {
                    positions.push(cell);
                }
            }
            if placed || positions.len() < 2 {
                continue;
            }

            if let Some(line) = aligned_row(&positions) {
                let eliminations =
                    eliminate_along(&ROW_UNITS[line], box_idx, board, bit, d, |c| box_of(c));
                if !eliminations.is_empty() {
                    let step = Step {
                        technique: TechniqueKind::LockedCandidatesPointing,
                        deductions: eliminations,
                        focus_cells: positions.clone(),
                        focus_house: Some(HouseRef {
                            kind: UnitKind::Box,
                            index: box_idx as u8,
                        }),
                    };
                    if !emit(step) {
                        return;
                    }
                    continue;
                }
            }
            if let Some(line) = aligned_col(&positions) {
                let eliminations =
                    eliminate_along(&COL_UNITS[line], box_idx, board, bit, d, |c| box_of(c));
                if !eliminations.is_empty() {
                    let step = Step {
                        technique: TechniqueKind::LockedCandidatesPointing,
                        deductions: eliminations,
                        focus_cells: positions,
                        focus_house: Some(HouseRef {
                            kind: UnitKind::Box,
                            index: box_idx as u8,
                        }),
                    };
                    if !emit(step) {
                        return;
                    }
                }
            }
        }
    }
}

pub fn find_claiming(board: &Board) -> Vec<Step> {
    let mut out = Vec::new();
    find_claiming_each(board, |s| {
        out.push(s);
        true
    });
    out
}

pub fn find_first_claiming(board: &Board) -> Option<Step> {
    let mut found = None;
    find_claiming_each(board, |s| {
        found = Some(s);
        false
    });
    found
}

fn find_claiming_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    for line_idx in 0..9usize {
        for (kind, unit) in [
            (UnitKind::Row, &ROW_UNITS[line_idx]),
            (UnitKind::Col, &COL_UNITS[line_idx]),
        ] {
            for d in 1u8..=9 {
                let bit = digit_to_bit(d);
                let mut placed = false;
                let mut positions: Vec<usize> = Vec::new();
                for &cell in unit {
                    if board.cell(cell) == d {
                        placed = true;
                        break;
                    }
                    if board.is_empty(cell) && board.candidates(cell) & bit != 0 {
                        positions.push(cell);
                    }
                }
                if placed || positions.len() < 2 {
                    continue;
                }
                let first_box = box_of(positions[0]);
                if positions.iter().all(|&c| box_of(c) == first_box) {
                    let eliminations =
                        eliminate_along(&BOX_UNITS[first_box], line_idx, board, bit, d, |c| {
                            match kind {
                                UnitKind::Row => row_of(c),
                                UnitKind::Col => col_of(c),
                                _ => unreachable!(),
                            }
                        });
                    if !eliminations.is_empty() {
                        let step = Step {
                            technique: TechniqueKind::LockedCandidatesClaiming,
                            deductions: eliminations,
                            focus_cells: positions,
                            focus_house: Some(HouseRef {
                                kind,
                                index: line_idx as u8,
                            }),
                        };
                        if !emit(step) {
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn aligned_row(positions: &[usize]) -> Option<usize> {
    let r = row_of(positions[0]);
    if positions.iter().all(|&c| row_of(c) == r) {
        Some(r)
    } else {
        None
    }
}

fn aligned_col(positions: &[usize]) -> Option<usize> {
    let c = col_of(positions[0]);
    if positions.iter().all(|&cell| col_of(cell) == c) {
        Some(c)
    } else {
        None
    }
}

fn eliminate_along(
    line: &[usize; 9],
    exclude_key: usize,
    board: &Board,
    bit: u16,
    digit: u8,
    key_of: impl Fn(usize) -> usize,
) -> Vec<Deduction> {
    let mut out = Vec::new();
    for &cell in line {
        if key_of(cell) == exclude_key {
            continue;
        }
        if board.is_empty(cell) && board.candidates(cell) & bit != 0 {
            out.push(Deduction::Eliminate { cell, digit });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(r: usize, c: usize) -> usize {
        r * 9 + c
    }

    #[test]
    fn pointing_eliminates_in_row() {
        let mut b = Board::empty();
        // Force digit 5 to only appear at R1C1 and R1C2 within box 0
        // by removing 5 from R2C1..R3C3 (the rest of box 0 minus R1C1, R1C2, R1C3)
        for &c in &[cell(0, 2), cell(1, 0), cell(1, 1), cell(1, 2), cell(2, 0), cell(2, 1), cell(2, 2)] {
            b.eliminate(c, 5);
        }
        let steps = find_pointing(&b);
        // 5 should be eliminatable from R1C3..R1C8 in row 0 (those that still have 5)
        let s = steps.iter().find(|s| s.technique == TechniqueKind::LockedCandidatesPointing).expect("expected a pointing step");
        // Eliminations should be on row 0, columns 3..=8, digit 5
        for d in &s.deductions {
            match d {
                Deduction::Eliminate { cell: c, digit: 5 } => {
                    assert_eq!(row_of(*c), 0);
                    assert!(col_of(*c) >= 3);
                }
                _ => panic!("unexpected deduction: {:?}", d),
            }
        }
    }

    #[test]
    fn claiming_eliminates_in_box() {
        let mut b = Board::empty();
        // Force digit 5 in row 0 to only appear at R1C1, R1C2, R1C3 (all in box 0)
        for c in 3..9 {
            b.eliminate(cell(0, c), 5);
        }
        let steps = find_claiming(&b);
        let _ = steps.iter().find(|s| s.technique == TechniqueKind::LockedCandidatesClaiming).expect("expected a claiming step");
    }
}
