//! Gateable technique engine for spec-driven generation, PoC scope = the ladder
//! up to HiddenQuad.
//!
//! Unlike core's solver this records NOTHING for play (no `Step`, no
//! `Deduction`, no `focus_cells`): each technique mutates the board and the
//! engine reports only which KIND fired, which is all the spec gates need. The
//! technique bodies are the faithful up-to-HiddenQuad copies from solver-lab
//! (same scan order as core, so the accept/reject trajectory matches); the new
//! layer is the kind-gated [`step_once`] plus the two spec primitives built on
//! it: [`solve_tracked`] (baseline gate + requirement counts) and
//! [`min_target_uses`] (verify's avoid-target walk).
//!
//! Kinds are indexed in difficulty order; the index IS the kind and the bit
//! `1 << index` is its membership bit in a [`Mask`]. See [`crate::spec`].

use crate::grid::{
    BOX_UNITS, Board, COL_UNITS, CELLS, ROW_UNITS, UnitKind, all_units, box_of, col_of,
    digit_to_bit, iter_digits, popcount, row_of,
};
use crate::util::for_each_combination;

/// Number of technique kinds in PoC scope (NakedSingle .. HiddenQuad).
pub const NUM: usize = 10;

// Kind indices, difficulty order. The index is the kind; `1 << index` is its
// membership bit in a `Mask`.
pub const NAKED_SINGLE: usize = 0;
pub const HIDDEN_SINGLE: usize = 1;
pub const LC_POINTING: usize = 2;
pub const LC_CLAIMING: usize = 3;
pub const NAKED_PAIR: usize = 4;
pub const HIDDEN_PAIR: usize = 5;
pub const NAKED_TRIPLE: usize = 6;
pub const HIDDEN_TRIPLE: usize = 7;
pub const NAKED_QUAD: usize = 8;
pub const HIDDEN_QUAD: usize = 9;

/// Difficulty of each kind, matching core's REGISTRY.
pub const DIFFICULTY: [u32; NUM] = [10, 15, 20, 25, 30, 33, 36, 39, 42, 45];

/// Human-readable names, for bench/diagnostic output.
pub const NAMES: [&str; NUM] = [
    "naked-single",
    "hidden-single",
    "lc-pointing",
    "lc-claiming",
    "naked-pair",
    "hidden-pair",
    "naked-triple",
    "hidden-triple",
    "naked-quad",
    "hidden-quad",
];

/// A set of technique kinds as a bitmask (`1 << kind_index`).
pub type Mask = u32;

/// Each entry applies its kind's FIRST applicable step to the board, returning
/// whether it changed anything. Difficulty order — index = kind.
const STEPS: [fn(&mut Board) -> bool; NUM] = [
    naked_single,
    hidden_single,
    lc_pointing,
    lc_claiming,
    naked_pair,
    hidden_pair,
    naked_triple,
    hidden_triple,
    naked_quad,
    hidden_quad,
];

/// Apply the easiest in-`allowed` technique that fires, returning its kind
/// index, or `None` if no allowed technique applies (stuck).
#[inline]
fn step_once(b: &mut Board, allowed: Mask) -> Option<usize> {
    for idx in 0..NUM {
        if allowed & (1 << idx) != 0 && STEPS[idx](b) {
            return Some(idx);
        }
    }
    None
}

/// Outcome of a tracked baseline solve: whether the `allowed` toolbox solved the
/// board, and how many times each kind fired (for the requirement check).
pub struct Outcome {
    pub solved: bool,
    pub counts: [u16; NUM],
}

/// Solve `board` using only the `allowed` toolbox, easiest-first, tallying each
/// kind's firings. This is the strip loop's baseline gate AND its requirement
/// counter in one pass — core's `baseline_solve_tracked`.
///
/// Naked singles (the overwhelmingly most common step, and a cascade after every
/// harder elimination) are drained in BATCHED waves instead of one-per-full-scan,
/// killing core's quadratic. This only reorders single placements within the
/// naked-single priority band, so it is confluent with the easiest-first trace:
/// `solved` and every kind's count (hence `requirement_met`) are identical. The
/// boards reaching here passed the uniqueness gate, so every naked single is a
/// forced move of the unique solution and batch-placing can never conflict.
pub fn solve_tracked(board: &Board, allowed: Mask) -> Outcome {
    let mut b = board.clone();
    let mut counts = [0u16; NUM];
    let bat_singles = allowed & (1 << NAKED_SINGLE) != 0;
    loop {
        if bat_singles {
            let n = drain_naked_singles(&mut b);
            if n > 0 {
                counts[NAKED_SINGLE] = counts[NAKED_SINGLE].saturating_add(n);
                continue;
            }
        }
        if b.is_solved() {
            return Outcome { solved: true, counts };
        }
        // No naked singles left: take one step from the rest of the ladder.
        // Mask naked single off when we already drained it, so we don't waste a
        // full 81-cell rescan that is guaranteed to find nothing.
        let rest = if bat_singles { allowed & !(1 << NAKED_SINGLE) } else { allowed };
        match step_once(&mut b, rest) {
            Some(idx) => counts[idx] = counts[idx].saturating_add(1),
            None => return Outcome { solved: false, counts },
        }
    }
}

/// Place every naked single, in repeated waves, until none remain. Returns how
/// many were placed. Placing during the scan lets a freshly-created single later
/// in the same pass be caught immediately; the outer wave loop catches ones
/// created earlier in the pass.
#[inline]
fn drain_naked_singles(b: &mut Board) -> u16 {
    let mut placed = 0u16;
    loop {
        let mut found = false;
        for i in 0..CELLS {
            if !b.is_empty(i) {
                continue;
            }
            let m = b.candidates(i);
            if popcount(m) == 1 {
                let d = iter_digits(m).next().unwrap();
                b.place(i, d);
                placed = placed.saturating_add(1);
                found = true;
            }
        }
        if !found {
            return placed;
        }
    }
}

/// Minimum number of times a `target` technique is *forced* to fire to solve
/// `board` given the `scope` toolbox — core's verify avoid-target walk. Prefers
/// any non-target in-scope step; only takes a target step when stuck on
/// non-targets, counting those. If the result is `>= required`, the target is
/// irreplaceable (cannot be substituted by the rest of the in-scope toolbox).
///
/// `scope` is the full in-scope set (including the target); `target` is the
/// bit(s) of the technique whose necessity is being proven.
pub fn min_target_uses(board: &Board, scope: Mask, target: Mask) -> usize {
    let mut b = board.clone();
    let non_target = scope & !target;
    let mut count = 0usize;
    loop {
        if b.is_solved() {
            return count;
        }
        if step_once(&mut b, non_target).is_some() {
            continue;
        }
        // Stuck on non-targets: the only new steps `scope` adds are target ones.
        match step_once(&mut b, scope) {
            None => return count, // stuck entirely
            Some(idx) => {
                if target & (1 << idx) != 0 {
                    count += 1;
                }
            }
        }
    }
}

// --- technique bodies (faithful up-to-HiddenQuad copies; scan order matches
// core, so the strip trajectory is identical) ---------------------------------

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

fn naked_pair(b: &mut Board) -> bool {
    naked_subset(b, 2)
}
fn naked_triple(b: &mut Board) -> bool {
    naked_subset(b, 3)
}
fn naked_quad(b: &mut Board) -> bool {
    naked_subset(b, 4)
}
fn hidden_pair(b: &mut Board) -> bool {
    hidden_subset(b, 2)
}
fn hidden_triple(b: &mut Board) -> bool {
    hidden_subset(b, 3)
}
fn hidden_quad(b: &mut Board) -> bool {
    hidden_subset(b, 4)
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
