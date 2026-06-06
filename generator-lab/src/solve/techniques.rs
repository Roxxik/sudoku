//! The composable technique bodies the [`LogicSolver`](super::LogicSolver) ladder
//! drives — the up-to-HiddenQuad toolbox written ONCE over the
//! [`LogicBoard`](super::LogicBoard) contract (candidates + occupancy + the
//! [`Eliminate`](super::Eliminate) pruning primitive), so swapping the packing is a
//! change of type parameter, never a re-implementation.
//!
//! Each is the *first-applicable* form the easiest-first engine needs: it applies
//! exactly its kind's first firing step and returns whether it changed anything (so
//! the ladder honours the difficulty order — never two kinds in one "step"). The
//! faithful up-to-HiddenQuad bodies follow core's technique definitions, written over
//! the generic view.
//!
//! Reads work off *candidates only* — a digit already placed in a unit has left
//! every empty peer's candidates (the [`Marks`] invariant), so it never shows up in
//! a unit scan and needs no separate "is it placed here" check (the same trick
//! [`crate::probe::techniques`] uses).

use super::LogicBoard;
use crate::repr::{CELLS, CellIdx, Digit, Mark, UNITS};
use super::combinations::for_each_combination;

/// Cell `c`'s box index (0..9) — its band-of-three-rows times three plus its
/// stack-of-three-columns. The one piece of geometry the locked-candidates scans
/// need beyond [`UNITS`].
#[inline]
fn box_of(c: CellIdx) -> usize {
    (c / 9 / 3) * 3 + (c % 9) / 3
}

/// Whether the `positions` all share the same `key` (row or column); if so, the
/// common key. The alignment test locked-candidates (pointing) is built on.
#[inline]
fn aligned(positions: &[CellIdx], key: impl Fn(CellIdx) -> usize) -> Option<usize> {
    let k = key(positions[0]);
    positions.iter().all(|&c| key(c) == k).then_some(k)
}

/// **Naked single**: the first empty cell with exactly one candidate must hold it.
pub(super) fn naked_single<V: LogicBoard>(v: &mut V) -> bool {
    for c in 0..CELLS {
        let m = v.get(c);
        if m.len() == 1 {
            v.place(c, m.iter().next().expect("exactly one candidate"));
            return true;
        }
    }
    false
}

/// **Hidden single**: the first (unit, digit) where the digit has exactly one
/// candidate cell left in the unit — it is forced there.
pub(super) fn hidden_single<V: LogicBoard>(v: &mut V) -> bool {
    for unit in &UNITS {
        for di in 0..9 {
            let d = Digit::from_index(di);
            let mut only = None;
            let mut count = 0u32;
            for &c in unit {
                if v.get(c).contains(d) {
                    only = Some(c);
                    count += 1;
                    if count > 1 {
                        break;
                    }
                }
            }
            if count == 1 {
                v.place(only.expect("count == 1"), d);
                return true;
            }
        }
    }
    false
}

/// **Locked candidates (pointing)**: a digit confined to one line within a box is
/// eliminated from the rest of that line. Returns at the first firing.
pub(super) fn lc_pointing<V: LogicBoard>(v: &mut V) -> bool {
    for b in 0..9 {
        let box_unit = &UNITS[18 + b];
        for di in 0..9 {
            let d = Digit::from_index(di);
            let mut positions: Vec<CellIdx> = Vec::new();
            for &c in box_unit {
                if v.get(c).contains(d) {
                    positions.push(c);
                }
            }
            if positions.len() < 2 {
                continue;
            }
            if let Some(row) = aligned(&positions, |c| c / 9) {
                if eliminate_line_outside_box(v, &UNITS[row], b, d) {
                    return true;
                }
            }
            if let Some(col) = aligned(&positions, |c| c % 9) {
                if eliminate_line_outside_box(v, &UNITS[9 + col], b, d) {
                    return true;
                }
            }
        }
    }
    false
}

/// Clear `d` from every empty cell of `line` outside box `b`; report whether
/// anything was eliminated (pointing's payload).
#[inline]
fn eliminate_line_outside_box<V: LogicBoard>(
    v: &mut V,
    line: &[CellIdx; 9],
    b: usize,
    d: Digit,
) -> bool {
    let mut did = false;
    for &c in line {
        if box_of(c) == b {
            continue;
        }
        if v.get(c).contains(d) {
            v.eliminate(c, d);
            did = true;
        }
    }
    did
}

/// **Locked candidates (claiming)**: a digit confined to one box within a line is
/// eliminated from the rest of that box. `line` 0..9 are rows, 9..18 columns —
/// exactly [`UNITS`]'s line ordering.
pub(super) fn lc_claiming<V: LogicBoard>(v: &mut V) -> bool {
    for line in 0..18 {
        let line_unit = &UNITS[line];
        for di in 0..9 {
            let d = Digit::from_index(di);
            let mut positions: Vec<CellIdx> = Vec::new();
            for &c in line_unit {
                if v.get(c).contains(d) {
                    positions.push(c);
                }
            }
            if positions.len() < 2 {
                continue;
            }
            let first_box = box_of(positions[0]);
            if positions.iter().all(|&c| box_of(c) == first_box) {
                let mut did = false;
                for &c in &UNITS[18 + first_box] {
                    if on_line(c, line) {
                        continue;
                    }
                    if v.get(c).contains(d) {
                        v.eliminate(c, d);
                        did = true;
                    }
                }
                if did {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether cell `c` lies on `line` (a row for `line < 9`, a column otherwise) —
/// the cells claiming must NOT touch, since they are the source line.
#[inline]
fn on_line(c: CellIdx, line: usize) -> bool {
    if line < 9 { c / 9 == line } else { c % 9 == line - 9 }
}

/// **Naked subset** of `size`: `size` cells in a unit whose candidate union is
/// exactly `size` digits — those digits leave the other cells of the unit.
///
/// Each cell's candidates are read once per unit into a stack `[Mark; 9]` (bb's
/// `cand_masks`): `get` is the digit-major board's costly 9-board gather, so the
/// combination search must read the cache, never re-`get`. No `Vec` — units are a
/// fixed nine cells and a subset eliminates in place at the first firing.
pub(super) fn naked_subset<V: LogicBoard>(v: &mut V, size: usize) -> bool {
    for unit in &UNITS {
        let marks: [Mark; 9] = core::array::from_fn(|i| v.get(unit[i]));
        // Candidate slots: empty cells with 2..=size candidates (a filled cell reads
        // as the empty mark, so `len` 0 excludes it). `cand` holds slot indices 0..9.
        let mut cand = [0usize; 9];
        let mut n = 0;
        for i in 0..9 {
            let len = marks[i].len() as usize;
            if (2..=size).contains(&len) {
                cand[n] = i;
                n += 1;
            }
        }
        if n < size {
            continue;
        }
        let mut applied = false;
        for_each_combination(&cand[..n], size, |combo| {
            let union = combo.iter().fold(Mark::EMPTY, |acc, &k| acc | marks[k]);
            if union.len() as usize != size {
                return true; // not a subset — keep searching
            }
            // Eliminate the subset's digits from the unit's OTHER cells.
            let mut did = false;
            for i in 0..9 {
                if combo.contains(&i) {
                    continue;
                }
                for d in (marks[i] & union).iter() {
                    v.eliminate(unit[i], d);
                    did = true;
                }
            }
            applied = did;
            !did // stop once we eliminated something
        });
        if applied {
            return true;
        }
    }
    false
}

/// **Hidden subset** of `size`: `size` digits confined to the same `size` cells of
/// a unit — the other digits leave those cells. Caches the unit's per-cell marks
/// once (see [`naked_subset`]) and derives the per-digit position masks off them.
pub(super) fn hidden_subset<V: LogicBoard>(v: &mut V, size: usize) -> bool {
    for unit in &UNITS {
        let marks: [Mark; 9] = core::array::from_fn(|i| v.get(unit[i]));
        // Position mask (over the 9 unit-cell slots) per digit, for digits with
        // 2..=size candidate cells (a placed digit has none — see module doc).
        let mut positions = [0u16; 9];
        let mut digits = [0usize; 9];
        let mut n = 0;
        for di in 0..9 {
            let d = Digit::from_index(di);
            let mut pos = 0u16;
            for i in 0..9 {
                if marks[i].contains(d) {
                    pos |= 1 << i;
                }
            }
            let pc = pos.count_ones() as usize;
            if (2..=size).contains(&pc) {
                positions[di] = pos;
                digits[n] = di;
                n += 1;
            }
        }
        if n < size {
            continue;
        }
        let mut applied = false;
        for_each_combination(&digits[..n], size, |combo| {
            let union: u16 = combo.iter().map(|&di| positions[di]).fold(0, |a, x| a | x);
            if union.count_ones() as usize != size {
                return true;
            }
            // The combo digits stay; every other candidate leaves the union's cells.
            let keep = combo
                .iter()
                .fold(Mark::EMPTY, |mut acc, &di| {
                    acc.insert(Digit::from_index(di));
                    acc
                });
            let mut did = false;
            for i in 0..9 {
                if union & (1 << i) == 0 {
                    continue;
                }
                for d in marks[i].without(keep).iter() {
                    v.eliminate(unit[i], d);
                    did = true;
                }
            }
            applied = did;
            !did
        });
        if applied {
            return true;
        }
    }
    false
}
