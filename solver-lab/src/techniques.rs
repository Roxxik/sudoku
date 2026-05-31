//! Minimal baseline technique solver for `train(HiddenQuad)`.
//!
//! This is the generator's *fixed scenery* — it shapes which boards the
//! existence prober sees (a strip is kept only if it stays solvable by this
//! toolbox) and accounts for ~25% of generation time. It is NOT what we are
//! optimizing; it just has to reproduce core's accept/revert decision.
//!
//! Stripped down from `core`'s technique suite: no `Step`, no `Deduction`, no
//! `Spec`, no trace/usage tracking. Each technique finds its first applicable
//! step (in the same scan order as core) and applies it directly to the board;
//! [`baseline_solvable`] returns a single bool. That bool is all the strip loop
//! needs, and because these are all sound monotone deductions on a board that
//! has already passed the uniqueness gate, the result is order-independent — so
//! the exact application order is faithful to core but not load-bearing.
//!
//! Baseline set = every technique with difficulty <= HiddenQuad(45):
//! naked/hidden single, LC pointing/claiming, naked+hidden pair/triple/quad.

use crate::grid::{
    BOX_UNITS, Board, COL_UNITS, CELLS, ROW_UNITS, UnitKind, all_units, box_of, col_of,
    digit_to_bit, iter_digits, popcount, row_of,
};
use crate::util::for_each_combination;

/// True iff `board` can be solved using only techniques up to HiddenQuad.
///
/// Mirrors `core`'s `baseline_solve_tracked(.., allow_up_to(HiddenQuad)).solved`:
/// repeatedly apply the easiest applicable technique's first step until solved
/// or no technique applies.
pub fn baseline_solvable(board: &Board) -> bool {
    let mut b = board.clone();
    loop {
        if b.is_solved() {
            return true;
        }
        // Difficulty order, matching core's REGISTRY:
        // NakedSingle(10) HiddenSingle(15) LC-Pointing(20) LC-Claiming(25)
        // NakedPair(30) HiddenPair(33) NakedTriple(36) HiddenTriple(39)
        // NakedQuad(42) HiddenQuad(45).
        let progressed = naked_single(&mut b)
            || hidden_single(&mut b)
            || lc_pointing(&mut b)
            || lc_claiming(&mut b)
            || naked_subset(&mut b, 2)
            || hidden_subset(&mut b, 2)
            || naked_subset(&mut b, 3)
            || hidden_subset(&mut b, 3)
            || naked_subset(&mut b, 4)
            || hidden_subset(&mut b, 4);
        if !progressed {
            return false; // stuck without solving
        }
    }
}

/// Like [`baseline_solvable`] but also reports whether a HiddenQuad step fired
/// — i.e. whether this board's easiest-first baseline trace *uses* a hidden
/// quad. This is core's cheap `requirement_met` gate for `train(HiddenQuad)`.
///
/// Not on the bench hot path (the prober comparison only needs the bool); used
/// by the `find_quad` example to produce an actual hidden-quad puzzle. Returns
/// `(solved, used_hidden_quad)`.
pub fn baseline_outcome(board: &Board) -> (bool, bool) {
    let mut b = board.clone();
    let mut used_hq = false;
    loop {
        if b.is_solved() {
            return (true, used_hq);
        }
        if naked_single(&mut b)
            || hidden_single(&mut b)
            || lc_pointing(&mut b)
            || lc_claiming(&mut b)
            || naked_subset(&mut b, 2)
            || hidden_subset(&mut b, 2)
            || naked_subset(&mut b, 3)
            || hidden_subset(&mut b, 3)
            || naked_subset(&mut b, 4)
        {
            continue;
        }
        if hidden_subset(&mut b, 4) {
            used_hq = true;
            continue;
        }
        return (false, used_hq);
    }
}

fn naked_single(b: &mut Board) -> bool {
    for i in 0..CELLS {
        if !b.is_empty(i) {
            continue;
        }
        let mask = b.candidates(i);
        if popcount(mask) == 1 {
            let d = iter_digits(mask).next().unwrap();
            b.place(i, d);
            return true;
        }
    }
    false
}

fn hidden_single(b: &mut Board) -> bool {
    for (_, _, unit) in all_units() {
        // `once` = digits appearing in >=1 empty cell, `twice` = in >=2. A
        // hidden single is `once & !twice & !placed`.
        let mut placed = 0u16;
        let mut once = 0u16;
        let mut twice = 0u16;
        for &cell in unit {
            let v = b.cell(cell);
            if v != 0 {
                placed |= digit_to_bit(v);
            } else {
                let m = b.candidates(cell);
                twice |= once & m;
                once |= m;
            }
        }
        let singles = once & !twice & !placed;
        if singles != 0 {
            let bit = singles & singles.wrapping_neg();
            let d = bit.trailing_zeros() as u8 + 1;
            for &cell in unit {
                if b.is_empty(cell) && b.candidates(cell) & bit != 0 {
                    b.place(cell, d);
                    return true;
                }
            }
        }
    }
    false
}

fn naked_subset(b: &mut Board, size: usize) -> bool {
    let mut elims: Vec<(usize, u8)> = Vec::new();
    for (_, _, unit) in all_units() {
        let candidate_cells: Vec<usize> = unit
            .iter()
            .copied()
            .filter(|&c| {
                if !b.is_empty(c) {
                    return false;
                }
                let n = popcount(b.candidates(c)) as usize;
                n >= 2 && n <= size
            })
            .collect();
        if candidate_cells.len() < size {
            continue;
        }
        for_each_combination(&candidate_cells, size, |combo| {
            let union: u16 = combo.iter().map(|&c| b.candidates(c)).fold(0, |a, x| a | x);
            if popcount(union) != size as u32 {
                return true; // keep searching
            }
            for &cell in unit {
                if combo.contains(&cell) || !b.is_empty(cell) {
                    continue;
                }
                let to_remove = b.candidates(cell) & union;
                for d in iter_digits(to_remove) {
                    elims.push((cell, d));
                }
            }
            elims.is_empty() // stop (return false) once we have eliminations
        });
        if !elims.is_empty() {
            for (cell, d) in elims {
                b.eliminate(cell, d);
            }
            return true;
        }
    }
    false
}

fn hidden_subset(b: &mut Board, size: usize) -> bool {
    let mut elims: Vec<(usize, u8)> = Vec::new();
    for (_, _, unit) in all_units() {
        let mut positions: [u16; 10] = [0; 10];
        let mut digits_in_play: Vec<u8> = Vec::new();
        for d in 1u8..=9 {
            let bit = digit_to_bit(d);
            let mut placed = false;
            let mut pos: u16 = 0;
            for (i, &cell) in unit.iter().enumerate() {
                if b.cell(cell) == d {
                    placed = true;
                    break;
                }
                if b.is_empty(cell) && b.candidates(cell) & bit != 0 {
                    pos |= 1 << i;
                }
            }
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
            let union: u16 = combo.iter().map(|&d| positions[d as usize]).fold(0, |a, x| a | x);
            if union.count_ones() as usize != size {
                return true;
            }
            let keep_mask: u16 = combo.iter().map(|&d| digit_to_bit(d)).fold(0, |a, x| a | x);
            for i in 0..9 {
                if union & (1 << i) == 0 {
                    continue;
                }
                let cell = unit[i];
                let to_remove = b.candidates(cell) & !keep_mask;
                for d in iter_digits(to_remove) {
                    elims.push((cell, d));
                }
            }
            elims.is_empty()
        });
        if !elims.is_empty() {
            for (cell, d) in elims {
                b.eliminate(cell, d);
            }
            return true;
        }
    }
    false
}

fn lc_pointing(b: &mut Board) -> bool {
    for box_idx in 0..9usize {
        let unit = &BOX_UNITS[box_idx];
        for d in 1u8..=9 {
            let bit = digit_to_bit(d);
            let mut placed = false;
            let mut positions: Vec<usize> = Vec::new();
            for &cell in unit {
                if b.cell(cell) == d {
                    placed = true;
                    break;
                }
                if b.is_empty(cell) && b.candidates(cell) & bit != 0 {
                    positions.push(cell);
                }
            }
            if placed || positions.len() < 2 {
                continue;
            }
            if let Some(line) = aligned(&positions, row_of) {
                let elims = eliminate_along(&ROW_UNITS[line], box_idx, b, bit, box_of);
                if !elims.is_empty() {
                    apply(b, &elims, d);
                    return true;
                }
            }
            if let Some(line) = aligned(&positions, col_of) {
                let elims = eliminate_along(&COL_UNITS[line], box_idx, b, bit, box_of);
                if !elims.is_empty() {
                    apply(b, &elims, d);
                    return true;
                }
            }
        }
    }
    false
}

fn lc_claiming(b: &mut Board) -> bool {
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
                    if b.cell(cell) == d {
                        placed = true;
                        break;
                    }
                    if b.is_empty(cell) && b.candidates(cell) & bit != 0 {
                        positions.push(cell);
                    }
                }
                if placed || positions.len() < 2 {
                    continue;
                }
                let first_box = box_of(positions[0]);
                if positions.iter().all(|&c| box_of(c) == first_box) {
                    let key = match kind {
                        UnitKind::Row => row_of,
                        UnitKind::Col => col_of,
                        UnitKind::Box => unreachable!(),
                    };
                    let elims = eliminate_along(&BOX_UNITS[first_box], line_idx, b, bit, key);
                    if !elims.is_empty() {
                        apply(b, &elims, d);
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn aligned(positions: &[usize], key: impl Fn(usize) -> usize) -> Option<usize> {
    let k = key(positions[0]);
    if positions.iter().all(|&c| key(c) == k) {
        Some(k)
    } else {
        None
    }
}

fn eliminate_along(
    line: &[usize; 9],
    exclude_key: usize,
    board: &Board,
    bit: u16,
    key_of: impl Fn(usize) -> usize,
) -> Vec<usize> {
    let mut out = Vec::new();
    for &cell in line {
        if key_of(cell) == exclude_key {
            continue;
        }
        if board.is_empty(cell) && board.candidates(cell) & bit != 0 {
            out.push(cell);
        }
    }
    out
}

fn apply(b: &mut Board, cells: &[usize], d: u8) {
    for &cell in cells {
        b.eliminate(cell, d);
    }
}
