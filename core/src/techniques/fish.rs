use crate::board::{Board, UnitKind, box_of, digit_to_bit};
use crate::techniques::{Deduction, Step, TechniqueKind};
use crate::util::for_each_combination;

const MAX_FIN_PER_BASE: usize = 3;
const MAX_TOTAL_FINS: usize = 5;

pub fn find_x_wing(board: &Board) -> Vec<Step> {
    find_fish(board, 2, TechniqueKind::XWing)
}

pub fn find_swordfish(board: &Board) -> Vec<Step> {
    find_fish(board, 3, TechniqueKind::Swordfish)
}

pub fn find_jellyfish(board: &Board) -> Vec<Step> {
    find_fish(board, 4, TechniqueKind::Jellyfish)
}

fn find_fish(board: &Board, size: usize, kind: TechniqueKind) -> Vec<Step> {
    let mut out = Vec::new();
    for d in 1u8..=9 {
        find_oriented(board, d, size, kind, UnitKind::Row, &mut out);
        find_oriented(board, d, size, kind, UnitKind::Col, &mut out);
    }
    out
}

fn cell_at(base: UnitKind, b: usize, x: usize) -> usize {
    match base {
        UnitKind::Row => b * 9 + x,
        UnitKind::Col => x * 9 + b,
        UnitKind::Box => unreachable!(),
    }
}

fn find_oriented(
    board: &Board,
    digit: u8,
    size: usize,
    kind: TechniqueKind,
    base: UnitKind,
    out: &mut Vec<Step>,
) {
    let bit = digit_to_bit(digit);
    let mut positions: [u16; 9] = [0; 9];
    let mut viable_bases: Vec<usize> = Vec::new();

    for b in 0..9 {
        let mut placed = false;
        let mut pos: u16 = 0;
        for x in 0..9 {
            let cell = cell_at(base, b, x);
            if board.cell(cell) == digit {
                placed = true;
                break;
            }
            if board.is_empty(cell) && board.candidates(cell) & bit != 0 {
                pos |= 1 << x;
            }
        }
        if placed {
            continue;
        }
        positions[b] = pos;
        let n = pos.count_ones() as usize;
        if n >= 2 && n <= size {
            viable_bases.push(b);
        }
    }

    if viable_bases.len() < size {
        return;
    }

    for_each_combination(&viable_bases, size, |combo| {
        let union: u16 = combo.iter().map(|&b| positions[b]).fold(0, |a, b| a | b);
        if union.count_ones() as usize != size {
            return;
        }
        let mut eliminations = Vec::new();
        let mut focus_cells: Vec<usize> = Vec::new();
        for &b in combo {
            for x in 0..9 {
                if positions[b] & (1 << x) != 0 {
                    focus_cells.push(cell_at(base, b, x));
                }
            }
        }
        for x in 0..9 {
            if union & (1 << x) == 0 {
                continue;
            }
            for y in 0..9 {
                if combo.contains(&y) {
                    continue;
                }
                let cell = cell_at(base, y, x);
                if !board.is_empty(cell) {
                    continue;
                }
                if board.candidates(cell) & bit == 0 {
                    continue;
                }
                eliminations.push(Deduction::Eliminate { cell, digit });
            }
        }
        if eliminations.is_empty() {
            return;
        }

        out.push(Step {
            technique: kind,
            deductions: eliminations,
            focus_cells,
            focus_house: None,
        });
    });
}

pub fn find_finned_x_wing(board: &Board) -> Vec<Step> {
    find_finned_fish(board, 2, TechniqueKind::FinnedXWing)
}

pub fn find_finned_swordfish(board: &Board) -> Vec<Step> {
    find_finned_fish(board, 3, TechniqueKind::FinnedSwordfish)
}

pub fn find_finned_jellyfish(board: &Board) -> Vec<Step> {
    find_finned_fish(board, 4, TechniqueKind::FinnedJellyfish)
}

fn find_finned_fish(board: &Board, size: usize, kind: TechniqueKind) -> Vec<Step> {
    let mut out = Vec::new();
    for d in 1u8..=9 {
        find_finned_oriented(board, d, size, kind, UnitKind::Row, &mut out);
        find_finned_oriented(board, d, size, kind, UnitKind::Col, &mut out);
    }
    out
}

fn find_finned_oriented(
    board: &Board,
    digit: u8,
    size: usize,
    kind: TechniqueKind,
    base: UnitKind,
    out: &mut Vec<Step>,
) {
    let bit = digit_to_bit(digit);
    let mut positions: [u16; 9] = [0; 9];
    let mut viable_bases: Vec<usize> = Vec::new();

    for b in 0..9 {
        let mut placed = false;
        let mut pos: u16 = 0;
        for x in 0..9 {
            let cell = cell_at(base, b, x);
            if board.cell(cell) == digit {
                placed = true;
                break;
            }
            if board.is_empty(cell) && board.candidates(cell) & bit != 0 {
                pos |= 1 << x;
            }
        }
        if placed {
            continue;
        }
        positions[b] = pos;
        let n = pos.count_ones() as usize;
        if n >= 2 && n <= size + MAX_FIN_PER_BASE {
            viable_bases.push(b);
        }
    }

    if viable_bases.len() < size {
        return;
    }

    for_each_combination(&viable_bases, size, |combo| {
        let union: u16 = combo.iter().map(|&b| positions[b]).fold(0, |a, b| a | b);
        let union_count = union.count_ones() as usize;
        if union_count <= size {
            return;
        }
        if union_count > size + MAX_TOTAL_FINS {
            return;
        }

        let union_cols: Vec<usize> = (0..9).filter(|x| union & (1 << x) != 0).collect();
        for_each_combination(&union_cols, size, |cover| {
            let cover_mask: u16 = cover.iter().map(|&x| 1u16 << x).fold(0, |a, b| a | b);
            let fin_mask = union & !cover_mask;
            if fin_mask == 0 {
                return;
            }

            let mut fin_cells: Vec<usize> = Vec::new();
            for &b in combo {
                for x in 0..9 {
                    if positions[b] & (1 << x) != 0 && fin_mask & (1 << x) != 0 {
                        fin_cells.push(cell_at(base, b, x));
                    }
                }
            }
            if fin_cells.is_empty() || fin_cells.len() > MAX_TOTAL_FINS {
                return;
            }

            let fin_box = box_of(fin_cells[0]);
            if !fin_cells.iter().all(|&c| box_of(c) == fin_box) {
                return;
            }

            let mut eliminations = Vec::new();
            for &x in cover {
                for y in 0..9 {
                    if combo.contains(&y) {
                        continue;
                    }
                    let cell = cell_at(base, y, x);
                    if box_of(cell) != fin_box {
                        continue;
                    }
                    if !board.is_empty(cell) {
                        continue;
                    }
                    if board.candidates(cell) & bit == 0 {
                        continue;
                    }
                    eliminations.push(Deduction::Eliminate { cell, digit });
                }
            }
            if eliminations.is_empty() {
                return;
            }

            let mut focus_cells: Vec<usize> = Vec::new();
            for &b in combo {
                for x in 0..9 {
                    if positions[b] & (1 << x) != 0 {
                        focus_cells.push(cell_at(base, b, x));
                    }
                }
            }

            out.push(Step {
                technique: kind,
                deductions: eliminations,
                focus_cells,
                focus_house: None,
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;

    #[test]
    fn detects_basic_x_wing() {
        // Construct a board where digit 5 is restricted to columns 2 and 6
        // of rows 0 and 4 (and other rows have placements of 5 elsewhere).
        // Easier: hand-build a board where digit 5 candidates form an X-Wing.
        let mut b = Board::empty();
        // Eliminate 5 from row 0 columns except 2 and 6.
        for c in 0..9 {
            if c != 2 && c != 6 {
                b.eliminate(0 * 9 + c, 5);
            }
        }
        // Same for row 4.
        for c in 0..9 {
            if c != 2 && c != 6 {
                b.eliminate(4 * 9 + c, 5);
            }
        }
        // Have 5 still a candidate in some cells of cols 2 and 6 in other rows.
        // (It already is, since empty board starts with all candidates and we
        // only stripped row 0 and row 4 elsewhere.)
        let steps = find_x_wing(&b);
        // Expect at least one X-Wing step on digit 5, with eliminations in
        // cols 2 and 6 outside rows 0 and 4.
        let xw = steps.iter().find(|s| s.technique == TechniqueKind::XWing)
            .expect("expected an X-Wing on digit 5");
        // Eliminations should target cells in col 2 and col 6 in rows 1,2,3,5,6,7,8 — that's 14 cells.
        assert_eq!(xw.deductions.len(), 14);
    }
}
