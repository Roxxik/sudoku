use crate::board::{Board, UnitKind, box_of, digit_to_bit};
use crate::techniques::{Deduction, Step, TechniqueKind};
use crate::util::for_each_combination;

const MAX_FIN_PER_BASE: usize = 3;
const MAX_TOTAL_FINS: usize = 5;

pub fn find_x_wing(board: &Board) -> Vec<Step> {
    find_fish_all(board, 2, TechniqueKind::XWing)
}

pub fn find_swordfish(board: &Board) -> Vec<Step> {
    find_fish_all(board, 3, TechniqueKind::Swordfish)
}

pub fn find_jellyfish(board: &Board) -> Vec<Step> {
    find_fish_all(board, 4, TechniqueKind::Jellyfish)
}

pub fn find_first_x_wing(board: &Board) -> Option<Step> {
    find_fish_first(board, 2, TechniqueKind::XWing)
}

pub fn find_first_swordfish(board: &Board) -> Option<Step> {
    find_fish_first(board, 3, TechniqueKind::Swordfish)
}

pub fn find_first_jellyfish(board: &Board) -> Option<Step> {
    find_fish_first(board, 4, TechniqueKind::Jellyfish)
}

fn find_fish_each<F: FnMut(Step) -> bool>(
    board: &Board,
    size: usize,
    kind: TechniqueKind,
    mut emit: F,
) {
    let mut keep_going = true;
    'outer: for d in 1u8..=9 {
        for base in [UnitKind::Row, UnitKind::Col] {
            find_oriented_each(board, d, size, kind, base, |s| {
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

fn find_fish_all(board: &Board, size: usize, kind: TechniqueKind) -> Vec<Step> {
    let mut out = Vec::new();
    find_fish_each(board, size, kind, |s| {
        out.push(s);
        true
    });
    out
}

fn find_fish_first(board: &Board, size: usize, kind: TechniqueKind) -> Option<Step> {
    let mut found = None;
    find_fish_each(board, size, kind, |s| {
        found = Some(s);
        false
    });
    found
}

fn cell_at(base: UnitKind, b: usize, x: usize) -> usize {
    match base {
        UnitKind::Row => b * 9 + x,
        UnitKind::Col => x * 9 + b,
        UnitKind::Box => unreachable!(),
    }
}

fn find_oriented_each<F: FnMut(Step) -> bool>(
    board: &Board,
    digit: u8,
    size: usize,
    kind: TechniqueKind,
    base: UnitKind,
    mut emit: F,
) {
    let bit = digit_to_bit(digit);
    let mut positions: [u16; 9] = [0; 9];
    // viable_bases is at most 9 entries — stack-buffer to avoid a heap
    // alloc per call. find_oriented_each runs 9 digits × 2 orientations
    // per next_step in the hot path.
    let mut viable_bases_buf: [usize; 9] = [0; 9];
    let mut viable_len: usize = 0;

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
            viable_bases_buf[viable_len] = b;
            viable_len += 1;
        }
    }

    if viable_len < size {
        return;
    }
    let viable_bases = &viable_bases_buf[..viable_len];

    for_each_combination(viable_bases, size, |combo| {
        let union: u16 = combo.iter().map(|&b| positions[b]).fold(0, |a, b| a | b);
        if union.count_ones() as usize != size {
            return true;
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
            return true;
        }

        emit(Step {
            technique: kind,
            deductions: eliminations,
            focus_cells,
            focus_house: None,
        })
    });
}

pub fn find_finned_x_wing(board: &Board) -> Vec<Step> {
    find_finned_fish_all(board, 2, TechniqueKind::FinnedXWing)
}

pub fn find_finned_swordfish(board: &Board) -> Vec<Step> {
    find_finned_fish_all(board, 3, TechniqueKind::FinnedSwordfish)
}

pub fn find_finned_jellyfish(board: &Board) -> Vec<Step> {
    find_finned_fish_all(board, 4, TechniqueKind::FinnedJellyfish)
}

pub fn find_first_finned_x_wing(board: &Board) -> Option<Step> {
    find_finned_fish_first(board, 2, TechniqueKind::FinnedXWing)
}

pub fn find_first_finned_swordfish(board: &Board) -> Option<Step> {
    find_finned_fish_first(board, 3, TechniqueKind::FinnedSwordfish)
}

pub fn find_first_finned_jellyfish(board: &Board) -> Option<Step> {
    find_finned_fish_first(board, 4, TechniqueKind::FinnedJellyfish)
}

fn find_finned_fish_each<F: FnMut(Step) -> bool>(
    board: &Board,
    size: usize,
    kind: TechniqueKind,
    mut emit: F,
) {
    let mut keep_going = true;
    'outer: for d in 1u8..=9 {
        for base in [UnitKind::Row, UnitKind::Col] {
            find_finned_oriented_each(board, d, size, kind, base, |s| {
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

fn find_finned_fish_all(board: &Board, size: usize, kind: TechniqueKind) -> Vec<Step> {
    let mut out = Vec::new();
    find_finned_fish_each(board, size, kind, |s| {
        out.push(s);
        true
    });
    out
}

fn find_finned_fish_first(board: &Board, size: usize, kind: TechniqueKind) -> Option<Step> {
    let mut found = None;
    find_finned_fish_each(board, size, kind, |s| {
        found = Some(s);
        false
    });
    found
}

fn find_finned_oriented_each<F: FnMut(Step) -> bool>(
    board: &Board,
    digit: u8,
    size: usize,
    kind: TechniqueKind,
    base: UnitKind,
    mut emit: F,
) {
    let bit = digit_to_bit(digit);
    let mut positions: [u16; 9] = [0; 9];
    // Same stack-buffer pattern as find_oriented_each. Finned fish gets hit
    // hard in forced/needs benches because it's allowed in the filtered
    // walk; per-call Vec allocs showed up at ~15% in the profile.
    let mut viable_bases_buf: [usize; 9] = [0; 9];
    let mut viable_len: usize = 0;

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
            viable_bases_buf[viable_len] = b;
            viable_len += 1;
        }
    }

    if viable_len < size {
        return;
    }
    let viable_bases = &viable_bases_buf[..viable_len];

    let mut keep_going = true;
    for_each_combination(viable_bases, size, |combo| {
        let union: u16 = combo.iter().map(|&b| positions[b]).fold(0, |a, b| a | b);
        let union_count = union.count_ones() as usize;
        if union_count <= size {
            return true;
        }
        if union_count > size + MAX_TOTAL_FINS {
            return true;
        }

        // union_cols: up to 9 ints. Build in-place to avoid a per-outer-combo heap alloc.
        let mut union_cols_buf: [usize; 9] = [0; 9];
        let mut union_cols_len: usize = 0;
        for x in 0..9 {
            if union & (1 << x) != 0 {
                union_cols_buf[union_cols_len] = x;
                union_cols_len += 1;
            }
        }
        let union_cols = &union_cols_buf[..union_cols_len];
        for_each_combination(union_cols, size, |cover| {
            // Scalar accumulate: with a slice of unknown const-size, LLVM was
            // autovectorizing the iter().map().fold() into AVX-512 (vpmovqw,
            // vpsllvw…) and that setup cost dominated for size 2-4. A
            // straight loop is faster here per perf annotate.
            let mut cover_mask: u16 = 0;
            for &x in cover {
                cover_mask |= 1u16 << x;
            }
            let fin_mask = union & !cover_mask;
            if fin_mask == 0 {
                return true;
            }

            // fin_cells: at most MAX_TOTAL_FINS=5. Stack buffer.
            let mut fin_cells_buf: [usize; MAX_TOTAL_FINS + 1] = [0; MAX_TOTAL_FINS + 1];
            let mut fin_len: usize = 0;
            let mut overflowed = false;
            for &b in combo {
                for x in 0..9 {
                    if positions[b] & (1 << x) != 0 && fin_mask & (1 << x) != 0 {
                        if fin_len >= MAX_TOTAL_FINS {
                            overflowed = true;
                            break;
                        }
                        fin_cells_buf[fin_len] = cell_at(base, b, x);
                        fin_len += 1;
                    }
                }
                if overflowed {
                    break;
                }
            }
            if overflowed || fin_len == 0 {
                return true;
            }
            let fin_cells = &fin_cells_buf[..fin_len];

            let fin_box = box_of(fin_cells[0]);
            if !fin_cells.iter().all(|&c| box_of(c) == fin_box) {
                return true;
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
                return true;
            }

            let mut focus_cells: Vec<usize> = Vec::new();
            for &b in combo {
                for x in 0..9 {
                    if positions[b] & (1 << x) != 0 {
                        focus_cells.push(cell_at(base, b, x));
                    }
                }
            }

            let cont = emit(Step {
                technique: kind,
                deductions: eliminations,
                focus_cells,
                focus_house: None,
            });
            if !cont {
                keep_going = false;
            }
            cont
        });
        keep_going
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
