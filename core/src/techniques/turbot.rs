use crate::board::{
    BOX_UNITS, Board, CELLS, COL_UNITS, PEERS, ROW_UNITS, UnitKind, box_of, col_of,
    digit_to_bit, row_of,
};
use crate::techniques::{Deduction, Step, TechniqueKind};

// ===== Skyscraper =====

pub fn find_skyscraper(board: &Board) -> Vec<Step> {
    let mut out = Vec::new();
    find_skyscraper_each(board, |s| {
        out.push(s);
        true
    });
    out
}

pub fn find_first_skyscraper(board: &Board) -> Option<Step> {
    let mut found = None;
    find_skyscraper_each(board, |s| {
        found = Some(s);
        false
    });
    found
}

fn find_skyscraper_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    let mut keep_going = true;
    'outer: for d in 1u8..=9 {
        for base in [UnitKind::Row, UnitKind::Col] {
            find_skyscraper_oriented_each(board, d, base, |s| {
                let cont = emit(s);
                if !cont {
                    keep_going = false;
                }
                cont
            });
            if !keep_going {
                break 'outer;
            }
        }
    }
}

fn find_skyscraper_oriented_each<F: FnMut(Step) -> bool>(
    board: &Board,
    digit: u8,
    base: UnitKind,
    mut emit: F,
) {
    let conjugates = conjugate_pairs_along(board, digit, base);
    let cross = |c: usize| match base {
        UnitKind::Row => col_of(c),
        UnitKind::Col => row_of(c),
        UnitKind::Box => unreachable!(),
    };
    for i in 0..conjugates.len() {
        for j in (i + 1)..conjugates.len() {
            let (_, [c1a, c1b]) = conjugates[i];
            let (_, [c2a, c2b]) = conjugates[j];
            let x1a = cross(c1a);
            let x1b = cross(c1b);
            let x2a = cross(c2a);
            let x2b = cross(c2b);
            let n_shared = (x1a == x2a) as u8
                + (x1a == x2b) as u8
                + (x1b == x2a) as u8
                + (x1b == x2b) as u8;
            if n_shared != 1 {
                continue;
            }
            let (free1, free2) = if x1a == x2a {
                (c1b, c2b)
            } else if x1a == x2b {
                (c1b, c2a)
            } else if x1b == x2a {
                (c1a, c2b)
            } else {
                (c1a, c2a)
            };
            if let Some(step) = build_two_endpoint_elim(
                board,
                digit,
                TechniqueKind::Skyscraper,
                free1,
                free2,
                &[c1a, c1b, c2a, c2b],
            ) {
                if !emit(step) {
                    return;
                }
            }
        }
    }
}

// ===== 2-String Kite =====

pub fn find_two_string_kite(board: &Board) -> Vec<Step> {
    let mut out = Vec::new();
    find_two_string_kite_each(board, |s| {
        out.push(s);
        true
    });
    out
}

pub fn find_first_two_string_kite(board: &Board) -> Option<Step> {
    let mut found = None;
    find_two_string_kite_each(board, |s| {
        found = Some(s);
        false
    });
    found
}

fn find_two_string_kite_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    for d in 1u8..=9 {
        let row_pairs = conjugate_pairs_along(board, d, UnitKind::Row);
        let col_pairs = conjugate_pairs_along(board, d, UnitKind::Col);
        for &(_, [a1, a2]) in &row_pairs {
            for &(_, [b1, b2]) in &col_pairs {
                for &(ar, ar_other) in &[(a1, a2), (a2, a1)] {
                    for &(bc, bc_other) in &[(b1, b2), (b2, b1)] {
                        if ar == bc || ar == bc_other || ar_other == bc || ar_other == bc_other {
                            continue;
                        }
                        if box_of(ar) != box_of(bc) {
                            continue;
                        }
                        // ar must lie in col c_idx? No — ar is in row r_idx. bc is in col c_idx.
                        // They share a box. ar_other in row r_idx, bc_other in col c_idx.
                        // Free ends are ar_other and bc_other.
                        if let Some(step) = build_two_endpoint_elim(
                            board,
                            d,
                            TechniqueKind::TwoStringKite,
                            ar_other,
                            bc_other,
                            &[a1, a2, b1, b2],
                        ) {
                            if !emit(step) {
                                return;
                            }
                        }
                    }
                }
            }
        }
    }
}

// ===== Empty Rectangle =====

pub fn find_empty_rectangle(board: &Board) -> Vec<Step> {
    let mut out = Vec::new();
    find_empty_rectangle_each(board, |s| {
        out.push(s);
        true
    });
    out
}

pub fn find_first_empty_rectangle(board: &Board) -> Option<Step> {
    let mut found = None;
    find_empty_rectangle_each(board, |s| {
        found = Some(s);
        false
    });
    found
}

fn find_empty_rectangle_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    for d in 1u8..=9 {
        let bit = digit_to_bit(d);
        for box_idx in 0..9 {
            let box_cells = &BOX_UNITS[box_idx];
            let d_cells: Vec<usize> = box_cells
                .iter()
                .copied()
                .filter(|&c| board.is_empty(c) && board.candidates(c) & bit != 0)
                .collect();
            if d_cells.len() < 2 {
                continue;
            }
            // Try each (row arm, col arm) within the box.
            let box_rows: [usize; 3] = [row_of(box_cells[0]), row_of(box_cells[3]), row_of(box_cells[6])];
            let box_cols: [usize; 3] = [col_of(box_cells[0]), col_of(box_cells[1]), col_of(box_cells[2])];
            for &er_row in &box_rows {
                for &er_col in &box_cols {
                    // All d_cells must lie on er_row or er_col.
                    if !d_cells.iter().all(|&c| row_of(c) == er_row || col_of(c) == er_col) {
                        continue;
                    }
                    let has_row_arm = d_cells
                        .iter()
                        .any(|&c| row_of(c) == er_row && col_of(c) != er_col);
                    let has_col_arm = d_cells
                        .iter()
                        .any(|&c| col_of(c) == er_col && row_of(c) != er_row);
                    if !has_row_arm || !has_col_arm {
                        continue;
                    }

                    // Search for a conjugate pair in a column outside this box
                    // with one endpoint at (er_row, c'), other endpoint at (r_other, c')
                    // where r_other not in box_rows. Elim at (r_other, er_col).
                    for c_prime in 0..9 {
                        if box_cols.contains(&c_prime) {
                            continue;
                        }
                        let pair = conjugate_pair_in_unit(board, &COL_UNITS[c_prime], d);
                        let Some([cp1, cp2]) = pair else { continue };
                        let (r_anchor, r_other) = if row_of(cp1) == er_row {
                            (cp1, cp2)
                        } else if row_of(cp2) == er_row {
                            (cp2, cp1)
                        } else {
                            continue;
                        };
                        let r_other_row = row_of(r_other);
                        if box_rows.contains(&r_other_row) {
                            continue;
                        }
                        let elim_cell = r_other_row * 9 + er_col;
                        if !board.is_empty(elim_cell) {
                            continue;
                        }
                        if board.candidates(elim_cell) & bit == 0 {
                            continue;
                        }
                        let focus = {
                            let mut v: Vec<usize> = d_cells.clone();
                            v.push(r_anchor);
                            v.push(r_other);
                            v
                        };
                        let step = Step {
                            technique: TechniqueKind::EmptyRectangle,
                            deductions: vec![Deduction::Eliminate { cell: elim_cell, digit: d }],
                            focus_cells: focus,
                            focus_house: None,
                        };
                        if !emit(step) {
                            return;
                        }
                    }

                    // Symmetric: conjugate pair in a row outside the box with one endpoint at (r', er_col).
                    for r_prime in 0..9 {
                        if box_rows.contains(&r_prime) {
                            continue;
                        }
                        let pair = conjugate_pair_in_unit(board, &ROW_UNITS[r_prime], d);
                        let Some([cp1, cp2]) = pair else { continue };
                        let (c_anchor, c_other) = if col_of(cp1) == er_col {
                            (cp1, cp2)
                        } else if col_of(cp2) == er_col {
                            (cp2, cp1)
                        } else {
                            continue;
                        };
                        let c_other_col = col_of(c_other);
                        if box_cols.contains(&c_other_col) {
                            continue;
                        }
                        let elim_cell = er_row * 9 + c_other_col;
                        if !board.is_empty(elim_cell) {
                            continue;
                        }
                        if board.candidates(elim_cell) & bit == 0 {
                            continue;
                        }
                        let focus = {
                            let mut v: Vec<usize> = d_cells.clone();
                            v.push(c_anchor);
                            v.push(c_other);
                            v
                        };
                        let step = Step {
                            technique: TechniqueKind::EmptyRectangle,
                            deductions: vec![Deduction::Eliminate { cell: elim_cell, digit: d }],
                            focus_cells: focus,
                            focus_house: None,
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

// ===== shared helpers =====

fn conjugate_pairs_along(board: &Board, digit: u8, base: UnitKind) -> Vec<(usize, [usize; 2])> {
    let units: &[[usize; 9]; 9] = match base {
        UnitKind::Row => &ROW_UNITS,
        UnitKind::Col => &COL_UNITS,
        UnitKind::Box => unreachable!(),
    };
    let mut out = Vec::new();
    for (idx, unit) in units.iter().enumerate() {
        if let Some(pair) = conjugate_pair_in_unit(board, unit, digit) {
            out.push((idx, pair));
        }
    }
    out
}

fn conjugate_pair_in_unit(board: &Board, unit: &[usize; 9], digit: u8) -> Option<[usize; 2]> {
    let bit = digit_to_bit(digit);
    let mut found: [usize; 2] = [0; 2];
    let mut count = 0;
    for &c in unit {
        if board.cell(c) == digit {
            return None;
        }
        if board.is_empty(c) && board.candidates(c) & bit != 0 {
            if count >= 2 {
                return None;
            }
            found[count] = c;
            count += 1;
        }
    }
    if count == 2 { Some(found) } else { None }
}

fn build_two_endpoint_elim(
    board: &Board,
    digit: u8,
    kind: TechniqueKind,
    free1: usize,
    free2: usize,
    focus_cells: &[usize],
) -> Option<Step> {
    let bit = digit_to_bit(digit);
    let mut eliminations = Vec::new();
    for c in 0..CELLS {
        if c == free1 || c == free2 {
            continue;
        }
        if !board.is_empty(c) {
            continue;
        }
        if board.candidates(c) & bit == 0 {
            continue;
        }
        if PEERS[free1].contains(&c) && PEERS[free2].contains(&c) {
            eliminations.push(Deduction::Eliminate { cell: c, digit });
        }
    }
    if eliminations.is_empty() {
        return None;
    }
    Some(Step {
        technique: kind,
        deductions: eliminations,
        focus_cells: focus_cells.to_vec(),
        focus_house: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;

    #[test]
    fn finds_basic_skyscraper() {
        // Digit 5: row 0 conjugate {c0, c3}, row 1 conjugate {c0, c5}.
        // Shared col = 0. Free ends (0,3) and (1,5) are both in box 1
        // (rows 0-2, cols 3-5), so eliminations apply inside that box.
        let mut b = Board::empty();
        let d: u8 = 5;
        for c in 0..9 {
            if c != 0 && c != 3 {
                b.eliminate(c, d);
            }
        }
        for c in 0..9 {
            if c != 0 && c != 5 {
                b.eliminate(9 + c, d);
            }
        }
        let steps = find_skyscraper(&b);
        let sky = steps
            .iter()
            .find(|s| s.technique == TechniqueKind::Skyscraper)
            .expect("expected a Skyscraper");
        // Box 1 minus the two free ends: cells (0,4)=4 and (0,5)=5 and (1,3)=12
        // and (1,4)=13 have d already removed; (2,3)=21, (2,4)=22, (2,5)=23 remain.
        let elim_cells: std::collections::HashSet<usize> = sky
            .deductions
            .iter()
            .filter_map(|d| match d {
                Deduction::Eliminate { cell, .. } => Some(*cell),
                _ => None,
            })
            .collect();
        for expected in [21usize, 22, 23] {
            assert!(elim_cells.contains(&expected), "missing elim at cell {}", expected);
        }
    }
}
