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
use crate::repr::{CELLS, CellIdx, Digit, Mark, PEER_MASK, UNITS};
use super::combinations::for_each_combination;

/// Whether cell `s` sees cell `c` (shares a row, column, or box) — one bit test on
/// the precomputed peer mask, the wing family's hot "is a peer of" check (replacing a
/// linear `PEERS[s].contains(&c)` scan over the 20-element peer list).
#[inline]
fn sees(s: CellIdx, c: CellIdx) -> bool {
    (PEER_MASK[s] >> c) & 1 != 0
}

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

/// Cell at base line `b`, cross position `x`, in the given fish orientation: a row
/// base maps `(row b, col x)`, a column base maps `(col b, row x)`. Row-major
/// `row * 9 + col` — the one piece of geometry the fish scan needs.
#[inline]
fn fish_cell(row_base: bool, b: usize, x: usize) -> CellIdx {
    if row_base { b * 9 + x } else { x * 9 + b }
}

/// **Basic fish** of `size` (X-Wing 2, Swordfish 3, Jellyfish 4): for one digit,
/// `size` base lines (all rows, or all columns) whose candidate cells for that digit
/// span exactly `size` cross-lines — the digit then leaves those cross-lines in every
/// *other* base line. The single-digit / Fish branch's first-applicable body, ported
/// from core's `fish::find_oriented_each` over the generic [`LogicBoard`] view.
/// Non-finned only (the basic fish the curriculum's Fish branch surfaces).
///
/// Reads candidates only: a base line where `digit` is already placed has it in no
/// candidate cell (the [`Marks`] invariant), so its position mask is empty and it
/// self-excludes — no separate "is it placed" check, the same trick the subset bodies
/// use. A placed cross-cell likewise reads empty, so the elimination scan skips it.
pub(super) fn fish<V: LogicBoard>(v: &mut V, size: usize) -> bool {
    for di in 0..9 {
        let d = Digit::from_index(di);
        // Two orientations: rows as base lines (cross = columns), then columns as base.
        for row_base in [true, false] {
            // Per base line, the 9-bit mask of cross-positions where `d` is a candidate.
            let mut positions = [0u16; 9];
            let mut bases = [0usize; 9];
            let mut n = 0;
            for b in 0..9 {
                let mut pos = 0u16;
                for x in 0..9 {
                    if v.get(fish_cell(row_base, b, x)).contains(d) {
                        pos |= 1 << x;
                    }
                }
                positions[b] = pos;
                let pc = pos.count_ones() as usize;
                if (2..=size).contains(&pc) {
                    bases[n] = b;
                    n += 1;
                }
            }
            if n < size {
                continue;
            }
            let mut applied = false;
            for_each_combination(&bases[..n], size, |combo| {
                let union: u16 = combo.iter().map(|&b| positions[b]).fold(0, |a, x| a | x);
                if union.count_ones() as usize != size {
                    return true; // not a cover of exactly `size` cross-lines — keep searching
                }
                // Eliminate `d` from the cover cross-lines in every NON-base line.
                let mut did = false;
                for x in 0..9 {
                    if union & (1 << x) == 0 {
                        continue;
                    }
                    for y in 0..9 {
                        if combo.contains(&y) {
                            continue;
                        }
                        let cell = fish_cell(row_base, y, x);
                        if v.get(cell).contains(d) {
                            v.eliminate(cell, d);
                            did = true;
                        }
                    }
                }
                applied = did;
                !did // stop once we eliminated something
            });
            if applied {
                return true;
            }
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

// --- Bivalue-chain branch: the wing family ------------------------------------
//
// All three wings reason over *bivalue* cells (exactly two candidates) and the peer
// relation. They share two helpers: the bivalue-cell list, and the common-peer
// elimination — "digit `d` leaves every empty cell that sees the given wing cells".
// Ported from core's `xy_wing` / `w_wing` modules over the generic [`LogicBoard`]
// view; sound (only valid eliminations) and first-applicable (apply the first wing
// that actually removes a candidate, then return), so the spec gate sees the wing
// as required exactly when core does.

/// The empty cells holding exactly `n` candidates, paired with their candidate set —
/// the wing scans' raw material (`n == 2` bivalues, `n == 3` the XYZ pivot). A filled
/// cell reads as the empty [`Mark`] (see module doc), so the `len` test alone would
/// exclude it; the `is_empty` guard keeps the intent explicit.
fn cells_with_n_candidates<V: LogicBoard>(v: &V, n: u32) -> Vec<(CellIdx, Mark)> {
    (0..CELLS)
        .filter(|&c| v.is_empty(c) && v.get(c).len() == n)
        .map(|c| (c, v.get(c)))
        .collect()
}

/// Eliminate digit `d` from every empty cell that is a peer of *all* of `must_see`
/// and is not one of `exclude`; report whether anything was removed. The shared
/// elimination step of the wing family — the deduced digit leaves every cell that
/// sees the wing endpoints it must.
fn eliminate_common_peers<V: LogicBoard>(
    v: &mut V,
    exclude: &[CellIdx],
    must_see: &[CellIdx],
    d: Digit,
) -> bool {
    // The cells seeing *every* `must_see` endpoint are the intersection of their peer
    // masks; the excluded wing cells are removed up front. Walking that handful of bits
    // (lowest cell first, so the elimination order matches the old 0..CELLS scan)
    // replaces an all-81-cells × all-endpoints `contains` sweep.
    let mut common = u128::MAX;
    for &s in must_see {
        common &= PEER_MASK[s];
    }
    for &e in exclude {
        common &= !(1u128 << e);
    }
    let mut did = false;
    while common != 0 {
        let c = common.trailing_zeros() as CellIdx;
        common &= common - 1;
        if v.is_empty(c) && v.get(c).contains(d) {
            v.eliminate(c, d);
            did = true;
        }
    }
    did
}

/// **XY-Wing**: a bivalue pivot `{X,Y}` with two bivalue wings `{X,Z}` and `{Y,Z}`,
/// each a peer of the pivot. Whichever digit the pivot takes, one wing is forced to
/// `Z`, so `Z` leaves every cell that sees *both* wings. The Bivalue branch's
/// first-applicable body, ported from core's `xy_wing::find_each`.
pub(super) fn xy_wing<V: LogicBoard>(v: &mut V) -> bool {
    let bivalues = cells_with_n_candidates(v, 2);
    for &(pivot, pcands) in &bivalues {
        for (ai, &(a, acands)) in bivalues.iter().enumerate() {
            if a == pivot || !sees(pivot, a) {
                continue;
            }
            // `a` must share exactly one digit (X) with the pivot; its other digit
            // is the candidate Z to eliminate, and Z must not itself be in the pivot.
            let shared = pcands & acands;
            if shared.len() != 1 {
                continue;
            }
            let z = acands.without(shared);
            if !(z & pcands).is_empty() {
                continue;
            }
            // The second wing must be exactly {Y, Z}: the pivot's other digit + Z.
            let required_b = pcands.without(shared) | z;
            for &(b, bcands) in bivalues.iter().skip(ai + 1) {
                if b == pivot || b == a || !sees(pivot, b) || bcands != required_b {
                    continue;
                }
                let zd = z.iter().next().expect("z is a single digit");
                if eliminate_common_peers(v, &[pivot, a, b], &[a, b], zd) {
                    return true;
                }
            }
        }
    }
    false
}

/// **XYZ-Wing**: a *trivalue* pivot `{X,Y,Z}` and two bivalue wings, each a peer of
/// the pivot with candidates a subset of the pivot's, sharing exactly one digit `Z`
/// and together covering all three pivot digits. `Z` then leaves every cell that
/// sees the pivot *and* both wings. Ported from core's `xy_wing::find_xyz_wing_each`.
pub(super) fn xyz_wing<V: LogicBoard>(v: &mut V) -> bool {
    let bivalues = cells_with_n_candidates(v, 2);
    let trivalues = cells_with_n_candidates(v, 3);
    for &(pivot, pcands) in &trivalues {
        // Bivalue peers of the pivot whose candidates lie inside the pivot's.
        let wings: Vec<(CellIdx, Mark)> = bivalues
            .iter()
            .copied()
            .filter(|&(c, cands)| sees(pivot, c) && cands.without(pcands).is_empty())
            .collect();
        if wings.len() < 2 {
            continue;
        }
        let mut fired = false;
        for_each_combination(&wings, 2, |combo| {
            let (a, acands) = combo[0];
            let (b, bcands) = combo[1];
            let shared = acands & bcands;
            // Exactly one shared digit (Z), and the two wings cover all three pivot
            // candidates — otherwise keep searching.
            if shared.len() != 1 || (acands | bcands) != pcands {
                return true;
            }
            let zd = shared.iter().next().expect("one shared digit");
            if eliminate_common_peers(v, &[pivot, a, b], &[pivot, a, b], zd) {
                fired = true;
                return false; // stop once we eliminated something
            }
            true
        });
        if fired {
            return true;
        }
    }
    false
}

/// **W-Wing**: two bivalue cells `X`, `Y` with the *same* candidates `{P,Q}` that are
/// not peers, joined by a conjugate pair on one digit (say `P`) in some unit — its
/// two cells seeing `X` and `Y` respectively. Then `Q` leaves every cell that sees
/// both `X` and `Y`. Ported from core's `w_wing::find_each` + `try_link`.
pub(super) fn w_wing<V: LogicBoard>(v: &mut V) -> bool {
    let bivalues = cells_with_n_candidates(v, 2);
    for (i, &(x, xcands)) in bivalues.iter().enumerate() {
        for &(y, ycands) in bivalues.iter().skip(i + 1) {
            if xcands != ycands || sees(x, y) {
                continue;
            }
            // Try each of the two shared digits as the strong-link (conjugate) digit;
            // the other is the one eliminated.
            let digits: Vec<Digit> = xcands.iter().collect();
            for di in 0..2 {
                if w_wing_link(v, x, y, digits[di], digits[1 - di]) {
                    return true;
                }
            }
        }
    }
    false
}

/// One W-Wing strong-link attempt: scan units for a conjugate pair on `link` (a unit
/// with exactly two cells holding it) whose endpoints see `x` and `y` respectively;
/// if found, `other` leaves every cell seeing both `x` and `y`. Returns whether it
/// eliminated anything (first firing unit wins).
fn w_wing_link<V: LogicBoard>(v: &mut V, x: CellIdx, y: CellIdx, link: Digit, other: Digit) -> bool {
    for unit in &UNITS {
        // The unit's cells that still hold `link` — must be exactly two (a conjugate
        // pair). A filled cell reads empty, so it never counts.
        let mut pair = [0usize; 2];
        let mut n = 0;
        for &c in unit {
            if v.get(c).contains(link) {
                if n < 2 {
                    pair[n] = c;
                }
                n += 1;
            }
        }
        if n != 2 {
            continue;
        }
        let (c1, c2) = (pair[0], pair[1]);
        if c1 == x || c1 == y || c2 == x || c2 == y {
            continue;
        }
        // One endpoint must see `x`, the other `y` (either assignment).
        let ends = if sees(c1, x) && sees(c2, y) {
            Some((c1, c2))
        } else if sees(c1, y) && sees(c2, x) {
            Some((c2, c1))
        } else {
            None
        };
        let Some((cx, cy)) = ends else { continue };
        if eliminate_common_peers(v, &[x, y, cx, cy], &[x, y], other) {
            return true;
        }
    }
    false
}
