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
use crate::spec::kinds::{JELLYFISH, KindMask, SWORDFISH, W_WING, X_WING, XYZ_WING, XY_WING};

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

/// One unit's **naked subset** of `size`, off its precomputed candidate marks: returns
/// whether it eliminated anything (the first firing). Shared verbatim by both
/// [`naked_subset`] entry paths (scalar fresh-marks and SIMT cached-marks), so the
/// combination/elimination body lives once. `#[inline]` so each call site specialises
/// on its `marks` provenance (a stack-local gather vs the shared cache slice), keeping
/// the read single-source exactly as the two hand-written bodies were.
///
/// `get` is the digit-major board's costly 9-board gather, so the combination search
/// reads `marks`, never re-`get`s. No `Vec` — a unit is a fixed nine cells.
#[inline]
fn naked_unit<V: LogicBoard>(v: &mut V, unit: &[CellIdx; 9], marks: &[Mark; 9], size: usize) -> bool {
    // Candidate slots: empty cells with 2..=size candidates (a filled cell reads as the
    // empty mark, so `len` 0 excludes it). `cand` holds slot indices 0..9.
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
        return false;
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
    applied
}

/// **Naked subset** of `size`: `size` cells in a unit whose candidate union is
/// exactly `size` digits — those digits leave the other cells of the unit.
///
/// `cache` / `memo` are the SIMT ladder's cross-stall optimisations, dispatched ONCE
/// here (not per unit, so the scalar arm carries none of their overhead):
/// - `cache` supplies the per-unit marks built ONCE per stall and shared by all three
///   naked sizes (see [`crate::solve::simt`]'s `SubsetCache`), so the gather is paid
///   once, not per size; the scalar solvers pass `None` and gather each unit fresh.
/// - `memo` is `(no_fire, ladder_bit)`: a unit verified no-fire on a board unchanged
///   since cannot newly fire this kind, so it is skipped; a clean full scan sets its
///   bit. First-fire order is preserved either way.
///
/// The fast path needs BOTH `cache` and `memo` (the SIMT ladder always supplies both);
/// any other combination takes the scalar fresh-gather arm.
pub(super) fn naked_subset<V: LogicBoard>(
    v: &mut V,
    size: usize,
    cache: Option<&[[Mark; 9]; 27]>,
    memo: Option<(&mut [u8; 27], u8)>,
) -> bool {
    match (cache, memo) {
        // SIMT: cached per-unit marks + cross-stall no-fire memo.
        (Some(c), Some((no_fire, ladder_bit))) => {
            let bit = 1u8 << ladder_bit;
            for (u, unit) in UNITS.iter().enumerate() {
                if no_fire[u] & bit != 0 {
                    subset_tally(false);
                    continue;
                }
                subset_tally(true);
                if naked_unit(v, unit, &c[u], size) {
                    return true;
                }
                no_fire[u] |= bit; // clean full scan: skip this unit until it changes
            }
            false
        }
        // Scalar: gather each unit's marks fresh, scan every unit.
        _ => {
            for unit in &UNITS {
                let marks: [Mark; 9] = core::array::from_fn(|i| v.get(unit[i]));
                if naked_unit(v, unit, &marks, size) {
                    return true;
                }
            }
            false
        }
    }
}

/// The basic-fish kinds, for the "does this toolbox have any fish at all" gate shared
/// by [`fish_step`] and the SIMT ladder's cached-position rebuild.
pub(super) const ANY_FISH: KindMask = (1 << X_WING) | (1 << SWORDFISH) | (1 << JELLYFISH);

/// Per-`(orientation, digit, base-line)` candidate-position masks for the basic fish:
/// bit `x` of `pos[o][d][b]` is set iff digit `d` is a candidate at base line `b`,
/// cross position `x` (orientation `o`: `0` = rows as base lines, `1` = columns). What
/// distinguishes X-Wing / Swordfish / Jellyfish is *how many* base lines combine into a
/// cover — the per-line masks themselves are size-independent, so [`fish_step`] scans
/// the board once and the three sizes share it instead of each rebuilding it. The scan
/// is the fish's read-heavy half (one [`get`] per cell — see [`FishPositions::scan`]).
///
/// A digit's whole search reads only that digit's two slices (`pos[*][d]`), so the
/// masks also memoize per digit: the SIMT ladder caches a `FishPositions` across
/// stalls in its [`crate::solve::simt::LadderMemo`] and refreshes only the digits its
/// band diff proved changed ([`rebuild_digit`](Self::rebuild_digit)), handing the
/// cache to [`fish_step`] in place of the scan.
pub(super) struct FishPositions {
    pos: [[[u16; 9]; 9]; 2],
}

impl FishPositions {
    /// All-empty masks — the placeholder behind a dead [`crate::solve::simt::LadderMemo`]
    /// (every digit marked stale, so nothing ever reads these zeros). The cross-stall
    /// per-digit rebuild now lives on [`UnitPositions`] (Step 2b: shared with the hidden
    /// subsets' per-unit masks so the column transpose is computed once, not twice).
    pub(super) const EMPTY: FishPositions = FishPositions { pos: [[[0; 9]; 9]; 2] };

    /// Read every cell's candidates once into the per-line masks. The only board-reading
    /// pass the fish ladder makes per stall.
    ///
    /// One [`get`] per cell: a cell `(row, col)` holding `d` is candidate position `col`
    /// of base row `row` AND candidate position `row` of base col `col`, so a single read
    /// distributes into both orientations at once. (Reading per `(digit, orientation, b,
    /// x)` instead would fetch each cell's mask 18 times — once per digit per orientation —
    /// to extract one bit each.) A placed cell reads as the empty [`Mark`] (the [`Marks`]
    /// invariant), so it contributes nothing and the digit loop skips it for free.
    fn scan<V: LogicBoard>(v: &V) -> Self {
        let mut pos = [[[0u16; 9]; 9]; 2];
        // `row`/`col` are loop counters rather than `cell / 9` / `cell % 9` per cell, so
        // the per-cell magic divide-by-nine drops out of the one board-reading pass.
        let mut cell = 0;
        for row in 0..9 {
            for col in 0..9 {
                let m = v.get(cell);
                for d in m.iter() {
                    let di = d.index();
                    pos[0][di][row] |= 1 << col; // rows as base lines: cross position = col
                    pos[1][di][col] |= 1 << row; // columns as base lines: cross position = row
                }
                cell += 1;
            }
        }
        Self { pos }
    }
}

/// The SIMT ladder's SHARED cross-stall position cache (Step 2b): ONE struct holding BOTH
/// the basic fish's per-orientation masks ([`FishPositions`]) and the hidden subsets'
/// per-unit masks, refreshed by a single [`rebuild_digit`](Self::rebuild_digit) whose
/// column 9x9 transpose — the costly half, needed by both layouts — is computed ONCE per
/// dirty digit and scattered into both. Replaces 2a's two independent caches (each with
/// its own transpose + its own staleness mask); a perf A/B decides shared (this) vs
/// separate (see `generator-lab/docs/LADDER-REDO.md`).
///
/// The fish per-orientation masks ARE the hidden layout's row units (`fish.pos[0]`) and
/// column units (`fish.pos[1]`); only the box units (`subset[18..27]`) are unique to the
/// per-unit layout. The row/col data is held in BOTH shapes anyway so each consumer reads
/// contiguously — [`fish_step`] wants `fish.pos[o][di]` (per base line for a fixed digit),
/// [`hidden_subset`] wants `subset[u][di]` (per digit for a fixed unit); reading either out
/// of the other's layout would stride (the regression the bundled attempt hit — see the doc).
pub(super) struct UnitPositions {
    /// Fish per-orientation masks (also the hidden layout's row units 0..9 / column units
    /// 9..18). Refreshed only when a basic fish is in scope.
    pub(super) fish: FishPositions,
    /// Hidden subsets' per-unit masks: rows `u` 0..9, columns 9..18, boxes 18..27;
    /// `subset[u][di]` bit `i` = digit `di` is a candidate at unit `u`'s `i`-th cell
    /// (`UNITS[u][i]`). The row/col halves duplicate `fish`; only the boxes are unique.
    /// Refreshed only when a hidden subset is in scope.
    pub(super) subset: [[u16; 9]; 27],
}

impl UnitPositions {
    /// All-empty — the placeholder behind a dead [`crate::solve::simt::LadderMemo`] (every
    /// digit marked stale, so nothing ever reads these zeros).
    pub(super) const EMPTY: UnitPositions =
        UnitPositions { fish: FishPositions::EMPTY, subset: [[0; 9]; 27] };

    /// Refresh one digit's slices from its nine 9-bit row slices (`rows[r]` bit `c` = the
    /// digit is a candidate at cell `(r, c)`), scattering the SHARED row/col derivation into
    /// whichever consumer layouts are in scope. The column 9x9 bit transpose — the costly
    /// half — is computed ONCE here (built branchlessly: the scatter loop measurably raised
    /// branch-misses for the analogous transpose, see [`crate::solve::simt`]) and written to
    /// both layouts, instead of 2a's two separate rebuilds each recomputing it:
    /// - `want_fish`: the fish per-orientation masks (`fish.pos[0]` = rows, `[1]` = cols).
    /// - `want_hidden`: the per-unit masks — row units 0..9 ARE the row slices, column units
    ///   9..18 the transpose, box units 18..27 gather each box row's three-column segment at
    ///   the box's stack offset into the slot triplet (box slot `3*lr + lc` is cell
    ///   `(3*(b/3)+lr, 3*(b%3)+lc)`, the row-major box order of [`crate::repr::UNITS`]).
    ///
    /// Byte-identical to 2a's `FishPositions::rebuild_digit` + `simt::rebuild_subset_digit`
    /// (same bits, same slot order); only the transpose is now shared, not computed twice.
    pub(super) fn rebuild_digit(
        &mut self,
        di: usize,
        rows: [u16; 9],
        want_fish: bool,
        want_hidden: bool,
    ) {
        let mut cols = [0u16; 9];
        for (r, &row) in rows.iter().enumerate() {
            for (c, col) in cols.iter_mut().enumerate() {
                *col |= ((row >> c) & 1) << r;
            }
        }
        if want_fish {
            self.fish.pos[0][di] = rows;
            self.fish.pos[1][di] = cols;
        }
        if want_hidden {
            for r in 0..9 {
                self.subset[r][di] = rows[r];
            }
            for c in 0..9 {
                self.subset[9 + c][di] = cols[c];
            }
            for b in 0..9 {
                let br = 3 * (b / 3);
                let bc = 3 * (b % 3);
                self.subset[18 + b][di] = ((rows[br] >> bc) & 7)
                    | (((rows[br + 1] >> bc) & 7) << 3)
                    | (((rows[br + 2] >> bc) & 7) << 6);
            }
        }
    }
}

/// Try the three basic fishes in size-ascending order (X-Wing, then Swordfish, then
/// Jellyfish), sharing a single [`FishPositions`] across all three. Honours
/// `allowed` and preserves the exact first-applicable order of the three separate
/// `fish(b, size)` ladder entries it replaces (size outer, then digit, then
/// orientation), so the produced verdict and elimination set are byte-identical.
/// Returns the fired kind index, or `None`.
///
/// `memo` is the SIMT ladder's per-digit cross-stall memo: the no-fire verdict bytes
/// AND the cached positions, already refreshed for every changed digit (see
/// [`FishPositions`]) — so the memoized path never re-reads the board here. `None`
/// (the scalar solvers) scans the board fresh.
pub(super) fn fish_step<V: LogicBoard>(
    v: &mut V,
    allowed: KindMask,
    memo: Option<(&mut [u8; 9], &FishPositions)>,
) -> Option<usize> {
    if allowed & ANY_FISH == 0 {
        return None;
    }
    let scanned;
    let (mut nf, fp) = match memo {
        Some((nf, fp)) => (Some(nf), fp),
        None => {
            scanned = FishPositions::scan(v);
            (None, &scanned)
        }
    };
    // `slot` is the per-digit memo bit for this size (X-Wing 0 / Swordfish 1 / Jellyfish
    // 2), independent of the global kind index `bit`. `nf.as_deref_mut()` reborrows the
    // optional memo for each size's scan.
    for (bit, size, slot) in [(X_WING, 2usize, 0u8), (SWORDFISH, 3, 1), (JELLYFISH, 4, 2)] {
        if allowed & (1 << bit) != 0 {
            tech_checked(bit);
            if fish_sized(v, size, fp, slot, nf.as_deref_mut()) {
                return Some(bit);
            }
        }
    }
    None
}

/// Tally a harder technique getting its turn into [`crate::solve::simt`]'s per-kind
/// `TCHK` census (`bit` = the kind index) — a no-op off the `count`+native path. The fish
/// and wing bodies are shared between the SIMT ladder and the scalar solvers (verify's
/// avoid-walk among them), so the tally is **gated**: `tchk_gated` only counts while the
/// SIMT ladder is the driver, keeping the census a pure account of warp baseline servicing
/// (see `crate::solve::simt::tchk_gated`). SIMT-only, so `cfg`-gated out on wasm.
#[inline(always)]
fn tech_checked(_bit: usize) {
    #[cfg(all(feature = "count", not(target_arch = "wasm32")))]
    crate::solve::simt::tchk_gated(_bit);
}

/// Tally a fish per-digit scan into the SIMT harder-ladder memo counter (`run = false`
/// means the per-digit memo skipped it) — a no-op off the `count`+native path. The fish
/// memo is SIMT-only, so this reaches across to [`crate::solve::simt`]'s `LSTAT`; the
/// reference is `cfg`-gated out on wasm, where that module does not exist.
#[inline(always)]
fn fish_tally(_run: bool) {
    #[cfg(all(feature = "count", not(target_arch = "wasm32")))]
    crate::solve::simt::lstat_inc(if _run { 3 } else { 4 });
}

/// Tally a subset per-unit scan into the SIMT harder-ladder memo counter (`run = false`
/// means the per-unit memo skipped it) — the subset twin of [`fish_tally`] (LSTAT [1]
/// run / [2] skipped), reached only on the memoized path and `cfg`-gated out on wasm.
#[inline(always)]
fn subset_tally(_run: bool) {
    #[cfg(all(feature = "count", not(target_arch = "wasm32")))]
    crate::solve::simt::lstat_inc(if _run { 1 } else { 2 });
}

/// **Basic fish** of `size` (X-Wing 2, Swordfish 3, Jellyfish 4): for one digit,
/// `size` base lines (all rows, or all columns) whose candidate cells for that digit
/// span exactly `size` cross-lines — the digit then leaves those cross-lines in every
/// *other* base line. The single-digit / Fish branch's first-applicable body, ported
/// from core's `fish::find_oriented_each` over the generic [`LogicBoard`] view.
/// Non-finned only (the basic fish the curriculum's Fish branch surfaces). The
/// per-line candidate masks come precomputed in `fp` (see [`FishPositions`]).
///
/// Reads candidates only: a base line where `digit` is already placed has it in no
/// candidate cell (the [`Marks`] invariant), so its position mask is empty and it
/// self-excludes — no separate "is it placed" check, the same trick the subset bodies
/// use. A placed cross-cell likewise reads empty, so the elimination scan skips it.
fn fish_sized<V: LogicBoard>(
    v: &mut V,
    size: usize,
    fp: &FishPositions,
    slot: u8,
    mut memo: Option<&mut [u8; 9]>,
) -> bool {
    // Per-digit cross-stall memo (SIMT only): a basic fish reads one digit's positions
    // alone, so a digit unchanged since this size scanned it clean cannot newly fire —
    // skip it, first-fire order preserved (a skipped digit could only have returned
    // false). `None` = no memo (scalar solvers, full scan).
    let slot_bit = 1u8 << slot;
    for di in 0..9 {
        if let Some(nf) = memo.as_deref_mut() {
            if nf[di] & slot_bit != 0 {
                fish_tally(false);
                continue;
            }
            fish_tally(true);
        }
        let d = Digit::from_index(di);
        for (o, row_base) in [true, false].into_iter().enumerate() {
            // Per base line, the 9-bit mask of cross-positions where `d` is a candidate.
            // Keep the viable base lines and their masks side by side so the combination
            // search reads `posv[i]` directly instead of re-indexing `positions[bases[i]]`.
            let positions = &fp.pos[o][di];
            let mut posv = [0u16; 9];
            // Cover base lines are excluded from the elimination scan by a bitmask
            // (`base_mask`), so carry each viable line's bit alongside its position mask
            // and union both incrementally down the combination search — the bit set is
            // the linear `combo.contains(&y)` scan turned into one `& (1 << y)` test.
            let mut basebit = [0u16; 9];
            let mut n = 0;
            for b in 0..9 {
                let pc = positions[b].count_ones() as usize;
                if (2..=size).contains(&pc) {
                    posv[n] = positions[b];
                    basebit[n] = 1 << b;
                    n += 1;
                }
            }
            if n < size {
                continue;
            }
            // Search for `size` base lines whose candidate positions union to exactly
            // `size` cross-lines (a valid cover). The union is built incrementally down
            // the nested loops, and any prefix that already spans more than `size`
            // cross-lines is pruned — the union only grows, so an over-spilled prefix can
            // never tighten back to an exact cover, and the whole sub-tree under it is
            // dead. (The previous code re-folded the full `size`-element union per combo
            // and only rejected at the leaf, visiting every pruned sub-tree in full.) The
            // first cover that eliminates anything wins, honouring the first-applicable
            // contract; pruned covers are exactly those the leaf check would reject with
            // `count != size`, so the elimination set is byte-identical.
            match size {
                2 => {
                    for i in 0..n {
                        for j in i + 1..n {
                            let union = posv[i] | posv[j];
                            if union.count_ones() != 2 {
                                continue;
                            }
                            let base_mask = basebit[i] | basebit[j];
                            if fish_eliminate(v, d, row_base, positions, base_mask, union) {
                                return true;
                            }
                        }
                    }
                }
                3 => {
                    for i in 0..n {
                        for j in i + 1..n {
                            let uij = posv[i] | posv[j];
                            if uij.count_ones() > 3 {
                                continue; // prune: this pair already over-spills the cover
                            }
                            let bmij = basebit[i] | basebit[j];
                            for k in j + 1..n {
                                let union = uij | posv[k];
                                if union.count_ones() != 3 {
                                    continue;
                                }
                                let base_mask = bmij | basebit[k];
                                if fish_eliminate(v, d, row_base, positions, base_mask, union) {
                                    return true;
                                }
                            }
                        }
                    }
                }
                _ => {
                    // size == 4 (Jellyfish); the ladder never asks for a larger fish.
                    for i in 0..n {
                        for j in i + 1..n {
                            let uij = posv[i] | posv[j];
                            if uij.count_ones() > 4 {
                                continue; // prune the whole (k, l) sub-tree
                            }
                            let bmij = basebit[i] | basebit[j];
                            for k in j + 1..n {
                                let uijk = uij | posv[k];
                                if uijk.count_ones() > 4 {
                                    continue; // prune the whole l sub-tree
                                }
                                let bmijk = bmij | basebit[k];
                                for l in k + 1..n {
                                    let union = uijk | posv[l];
                                    if union.count_ones() != 4 {
                                        continue;
                                    }
                                    let base_mask = bmijk | basebit[l];
                                    if fish_eliminate(v, d, row_base, positions, base_mask, union) {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Both orientations scanned clean for digit di at this size: record it so a later
        // stall that leaves di unchanged skips the rescan.
        if let Some(nf) = memo.as_deref_mut() {
            nf[di] |= slot_bit;
        }
    }
    false
}

/// Eliminate `d` from the cover cross-lines (`union`) in every base line NOT in the
/// cover (`base_mask`'s bits are the cover lines). Returns whether anything was removed.
///
/// The cells to clear are read straight off `positions` — the same per-base-line
/// candidate masks `fish_sized` already searched (`positions[y] == fp.pos[o][di][y]`),
/// not by re-`get`ting each cover cell: for a non-cover base line `y`, `positions[y] &
/// union` is exactly the cross-positions where `d` is a candidate *and* in the cover, so
/// those are the eliminations. This both replaces the scattered per-cell `get`/`contains`
/// gather with a mask-and over already-loaded data and only walks cells that actually
/// hold `d`. `positions` is consistent with the board here: only a *successful* call
/// mutates `v`, and the first such call ends the whole fish search, so no earlier
/// elimination can have staled the masks. The cell-index geometry (a row base maps
/// `(y, x) -> y * 9 + x`, a column base maps `(x, y) -> x * 9 + y`) reduces to a
/// base/stride pair that is loop-invariant in `y`, so the orientation branch is hoisted
/// out of the inner walk.
fn fish_eliminate<V: LogicBoard>(
    v: &mut V,
    d: Digit,
    row_base: bool,
    positions: &[u16; 9],
    base_mask: u16,
    union: u16,
) -> bool {
    let mut did = false;
    for y in 0..9 {
        if base_mask & (1 << y) != 0 {
            continue;
        }
        // Cells of base line `y` at cover cross-positions that still hold `d`.
        let mut elim = positions[y] & union;
        // fish_cell(row_base, y, x): a row base walks cells (y, x) -> cross x strides by 1
        // within row y; a column base walks (x, y) -> cross x strides by a full row (9).
        let (base, stride) = if row_base { (y * 9, 1) } else { (y, 9) };
        while elim != 0 {
            let x = elim.trailing_zeros() as usize;
            elim &= elim - 1;
            v.eliminate(base + x * stride, d);
            did = true;
        }
    }
    did
}

/// One unit's **hidden subset** of `size`, off its per-digit position masks alone:
/// returns whether it eliminated anything. The twin of [`naked_unit`] — shared verbatim
/// by both [`hidden_subset`] entry paths, `#[inline]` so each call site specialises on
/// its data provenance. A hidden subset is a purely digit-major technique: the position
/// masks `positions[di]` ARE the unit's candidacy (bit `i` set iff digit `di` is a
/// candidate of slot `i`), so both the search and the elimination read them directly,
/// with no per-cell marks (the last cell-major dependency the redo folds out — see
/// `generator-lab/docs/LADDER-REDO.md`).
#[inline]
fn hidden_unit<V: LogicBoard>(
    v: &mut V,
    unit: &[CellIdx; 9],
    positions: &[u16; 9],
    size: usize,
) -> bool {
    // One pass over the digits classifies each by its candidate-cell count: those with
    // 2..=size cells are the subset-search candidates (`digits`); every digit with >=1 cell
    // is `present` (an unplaced digit — a placed one has no candidate cell). `present` is the
    // elimination's search space below, gathered here for free alongside the count the search
    // already needs.
    let mut digits = [0usize; 9];
    let mut n = 0;
    let mut present = 0u16;
    for di in 0..9 {
        let pc = positions[di].count_ones() as usize;
        present |= ((pc != 0) as u16) << di;
        if (2..=size).contains(&pc) {
            digits[n] = di;
            n += 1;
        }
    }
    if n < size {
        return false;
    }
    let mut applied = false;
    for_each_combination(&digits[..n], size, |combo| {
        let union: u16 = combo.iter().map(|&di| positions[di]).fold(0, |a, x| a | x);
        if union.count_ones() as usize != size {
            return true;
        }
        // The combo digits stay; every OTHER present digit leaves the cover cells it occupies.
        // Read straight off the masks (digit `di` sits at slot `i` iff `positions[di]` bit `i`
        // is set), so no marks: a non-combo digit's eliminations are exactly the cover cells
        // where it is still a candidate (`positions[di] & union`). Walk only the `present`
        // non-combo digits (placed/absent digits can eliminate nothing), and drive the loop BY
        // DIGIT (not by cell) so the common no-op cover stays cheap — a cover whose cells hold
        // only the combo digits has `positions[di] & union == 0` for every walked `di`, so the
        // inner bit-walk never runs (recovering the marks path's zero-iteration `without(keep)`
        // without the per-cell marks). Same (cell, digit) elimination SET as that path; the
        // emission order is digit-major here, but the fixpoint — and the per-bit writeback in
        // `ladder_step` — is order-independent.
        let combo_mask: u16 = combo.iter().fold(0, |a, &di| a | (1 << di));
        let mut elig = present & !combo_mask;
        let mut did = false;
        while elig != 0 {
            let di = elig.trailing_zeros() as usize;
            elig &= elig - 1;
            let mut hits = positions[di] & union;
            while hits != 0 {
                let i = hits.trailing_zeros() as usize;
                hits &= hits - 1;
                v.eliminate(unit[i], Digit::from_index(di));
                did = true;
            }
        }
        applied = did;
        !did
    });
    applied
}

/// **Hidden subset** of `size`: `size` digits confined to the same `size` cells of
/// a unit — the other digits leave those cells. A purely digit-major technique: it reads
/// only the per-digit position masks (`positions[di]` bit `i` = digit `di` is a candidate
/// of the unit's `i`-th cell), for both the search and the elimination (see [`hidden_unit`]).
///
/// `pos` / `memo` are the SIMT ladder's cross-stall optimisations, dispatched ONCE here
/// exactly as in [`naked_subset`] (so the scalar arm carries none of their overhead):
/// `pos` supplies the per-unit digit-position masks built ONCE per stall and shared across
/// the three hidden sizes (the memo's [`UnitPositions::subset`], rebuilt straight from the
/// digit-major bands — see [`crate::solve::simt`]); `memo` no-fire-gates per unit. The
/// scalar solvers pass both `None` and derive the position transpose fresh per unit; the
/// fast path needs both.
pub(super) fn hidden_subset<V: LogicBoard>(
    v: &mut V,
    size: usize,
    pos: Option<&[[u16; 9]; 27]>,
    memo: Option<(&mut [u8; 27], u8)>,
) -> bool {
    match (pos, memo) {
        // SIMT: cached per-unit positions + cross-stall no-fire memo.
        (Some(p), Some((no_fire, ladder_bit))) => {
            let bit = 1u8 << ladder_bit;
            for (u, unit) in UNITS.iter().enumerate() {
                if no_fire[u] & bit != 0 {
                    subset_tally(false);
                    continue;
                }
                subset_tally(true);
                if hidden_unit(v, unit, &p[u], size) {
                    return true;
                }
                no_fire[u] |= bit;
            }
            false
        }
        // Scalar: derive each unit's per-digit position transpose fresh. `positions[di]`
        // bit `i` is set iff digit `di` is a candidate of the unit's `i`-th cell.
        _ => {
            for unit in &UNITS {
                let mut positions = [0u16; 9];
                for i in 0..9 {
                    for d in v.get(unit[i]).iter() {
                        positions[d.index()] |= 1 << i;
                    }
                }
                if hidden_unit(v, unit, &positions, size) {
                    return true;
                }
            }
            false
        }
    }
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

/// Collect into `buf` the empty cells holding exactly `n` candidates, paired with their
/// candidate set, in ascending cell order; return how many. The wing scans' raw material
/// (`n == 2` bivalues, `n == 3` the XYZ pivot). A filled cell reads as the empty [`Mark`]
/// (see module doc), so the `len` test alone would exclude it; the `is_empty` guard keeps
/// the intent explicit. Writing into a caller stack buffer (the board has at most [`CELLS`]
/// such cells) avoids a per-stall heap allocation in the wing ladder.
fn cells_with_n_candidates<V: LogicBoard>(v: &V, n: u32, buf: &mut [(CellIdx, Mark); CELLS]) -> usize {
    let mut k = 0;
    for c in 0..CELLS {
        let m = v.get(c);
        if v.is_empty(c) && m.len() == n {
            buf[k] = (c, m);
            k += 1;
        }
    }
    k
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

/// Dense slot for a two-digit candidate mask (a bivalue cell's [`Mark`] bits): the
/// 36 possible 2-bit values of a 9-bit mask packed into `0..36`, ascending, so a
/// bivalue's candidate pair indexes a bucket directly. Non-2-bit masks map to slot 0
/// (never looked up — the wings only key on genuine bivalue pairs). Compile-time, so
/// the per-stall bucket build pays no wide-array cost.
const MASK_TO_SLOT: [u8; 512] = {
    let mut t = [0u8; 512];
    let (mut next, mut m) = (0u8, 0usize);
    while m < 512 {
        if (m as u16).count_ones() == 2 {
            t[m] = next;
            next += 1;
        }
        m += 1;
    }
    t
};

/// The inverse of [`MASK_TO_SLOT`]: each slot's two-digit mask, so the W-Wing can
/// recover its bucket's shared pair `{P,Q}` from the slot it is walking.
const SLOT_TO_MASK: [u16; 36] = {
    let mut t = [0u16; 36];
    let (mut next, mut m) = (0usize, 0usize);
    while m < 512 {
        if (m as u16).count_ones() == 2 {
            t[next] = m as u16;
            next += 1;
        }
        m += 1;
    }
    t
};

/// Bivalue cells grouped by their candidate pair: a CSR layout where each of the 36
/// two-digit masks owns a contiguous run of cells (in ascending cell order). This turns
/// the wings' "find the partner cells sharing this pair" from an O(n) rescan of every
/// bivalue into an O(1) bucket lookup, collapsing their O(n²) pair scans to O(Σ kᵢ²)
/// over the (tiny) per-pair buckets. Built once per [`wing_step`] stall from the shared
/// bivalue list; only the firing step mutates the board, so the buckets stay valid for
/// every non-firing wing attempt.
struct BivalueBuckets {
    /// Cells packed by slot, ascending within each slot (cell order preserved); only
    /// `[..bounds[36]]` is populated. A fixed array (the board has at most [`CELLS`]
    /// bivalue cells) keeps the whole per-stall build allocation-free.
    cells: [CellIdx; CELLS],
    /// `bounds[s]..bounds[s + 1]` is slot `s`'s run in `cells`.
    bounds: [u16; 37],
}

impl BivalueBuckets {
    /// One counting-sort pass over the bivalue list: tally each slot, prefix-sum into
    /// run bounds, then scatter cells into their slot (stable, so each run stays in
    /// cell order). The 37-wide bookkeeping is the only per-stall fixed cost.
    fn build(bivalues: &[(CellIdx, Mark)]) -> Self {
        let mut bounds = [0u16; 37];
        for &(_, m) in bivalues {
            bounds[MASK_TO_SLOT[m.bits() as usize] as usize + 1] += 1;
        }
        for i in 1..37 {
            bounds[i] += bounds[i - 1];
        }
        let mut cells = [0usize; CELLS];
        let mut cur = bounds;
        for &(c, m) in bivalues {
            let s = MASK_TO_SLOT[m.bits() as usize] as usize;
            cells[cur[s] as usize] = c;
            cur[s] += 1;
        }
        Self { cells, bounds }
    }

    /// The bivalue cells whose candidate pair is exactly `mask` (a 2-bit value), in
    /// cell order.
    #[inline]
    fn with_mask(&self, mask: u16) -> &[CellIdx] {
        self.run(MASK_TO_SLOT[mask as usize] as usize)
    }

    /// Slot `s`'s cell run.
    #[inline]
    fn run(&self, s: usize) -> &[CellIdx] {
        &self.cells[self.bounds[s] as usize..self.bounds[s + 1] as usize]
    }
}

/// Try the three bivalue-chain wings in difficulty order (XY-Wing, then XYZ-Wing, then
/// W-Wing), sharing a single bivalue-cell scan — and the [`BivalueBuckets`] grouping
/// built from it — across all three. The bivalue list is the wings' read-heavy raw
/// material; each wing body rebuilt it independently, so the all-81-cells scan ran up
/// to three times per stall. XY-Wing and W-Wing additionally find their partner cells
/// by candidate-pair bucket rather than rescanning the whole list. Honours `allowed`.
/// The reorder of the within-technique pair search is verdict-safe (the solve fixpoint
/// is confluent under elimination order — see `tests/confluence.rs`), and produces a
/// byte-identical generator fingerprint. A wing that fires returns immediately (the
/// ladder restarts). Returns the fired kind index, or `None`.
pub(super) fn wing_step<V: LogicBoard>(v: &mut V, allowed: KindMask) -> Option<usize> {
    const ANY_WING: KindMask = (1 << XY_WING) | (1 << XYZ_WING) | (1 << W_WING);
    const BUCKETED: KindMask = (1 << XY_WING) | (1 << W_WING);
    if allowed & ANY_WING == 0 {
        return None;
    }
    let mut bivbuf = [(0usize, Mark::EMPTY); CELLS];
    let nb = cells_with_n_candidates(v, 2, &mut bivbuf);
    let bivalues = &bivbuf[..nb];
    let buckets = (allowed & BUCKETED != 0).then(|| BivalueBuckets::build(bivalues));
    if allowed & (1 << XY_WING) != 0 {
        tech_checked(XY_WING);
        if xy_wing(v, bivalues, buckets.as_ref().expect("built")) {
            return Some(XY_WING);
        }
    }
    if allowed & (1 << XYZ_WING) != 0 {
        tech_checked(XYZ_WING);
        let mut tribuf = [(0usize, Mark::EMPTY); CELLS];
        let nt = cells_with_n_candidates(v, 3, &mut tribuf);
        let trivalues = &tribuf[..nt];
        if xyz_wing(v, bivalues, trivalues) {
            return Some(XYZ_WING);
        }
    }
    if allowed & (1 << W_WING) != 0 {
        tech_checked(W_WING);
        if w_wing(v, buckets.as_ref().expect("built")) {
            return Some(W_WING);
        }
    }
    None
}

/// **XY-Wing**: a bivalue pivot `{X,Y}` with two bivalue wings `{X,Z}` and `{Y,Z}`,
/// each a peer of the pivot. Whichever digit the pivot takes, one wing is forced to
/// `Z`, so `Z` leaves every cell that sees *both* wings. The Bivalue branch's
/// first-applicable body. `bivalues` drives the pivot loop in cell order;
/// [`BivalueBuckets`] supplies the two wings by candidate pair, so a pivot's wings are
/// found by direct bucket lookup instead of two nested rescans of the bivalue list.
fn xy_wing<V: LogicBoard>(v: &mut V, bivalues: &[(CellIdx, Mark)], buckets: &BivalueBuckets) -> bool {
    for &(pivot, pcands) in bivalues {
        // The pivot's two digits X (low) and Y (high) as singleton masks.
        let bits = pcands.bits();
        let x_bit = bits & bits.wrapping_neg();
        let y_bit = bits & (bits - 1);
        // Z ranges over the seven digits outside the pivot; the wings are then exactly
        // {X,Z} and {Y,Z}. Direct bucket lookup replaces the old shared-digit / required-b
        // rescan of every bivalue.
        let mut zbits = !bits & Mark::ALL.bits();
        while zbits != 0 {
            let z_bit = zbits & zbits.wrapping_neg();
            zbits &= zbits - 1;
            let zd = Digit::from_index(z_bit.trailing_zeros() as usize);
            // {X,Z}-wings and {Y,Z}-wings both seeing the pivot; whichever pair fires
            // eliminates Z from the cells seeing both wings (the pivot need not be seen).
            // The {Y,Z} bucket is independent of the {X,Z} wing, so look it up once.
            let bz = buckets.with_mask(y_bit | z_bit);
            if bz.is_empty() {
                continue;
            }
            for &a in buckets.with_mask(x_bit | z_bit) {
                if !sees(pivot, a) {
                    continue;
                }
                for &b in bz {
                    if !sees(pivot, b) {
                        continue;
                    }
                    if eliminate_common_peers(v, &[pivot, a, b], &[a, b], zd) {
                        return true;
                    }
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
/// `bivalues` / `trivalues` are the shared candidate-count lists (see [`wing_step`]).
fn xyz_wing<V: LogicBoard>(
    v: &mut V,
    bivalues: &[(CellIdx, Mark)],
    trivalues: &[(CellIdx, Mark)],
) -> bool {
    // Reused per-pivot scratch for the pivot's candidate wings (a stack buffer, refilled
    // each pivot, so the trivalue loop allocates nothing).
    let mut wingbuf = [(0usize, Mark::EMPTY); CELLS];
    for &(pivot, pcands) in trivalues {
        // Bivalue peers of the pivot whose candidates lie inside the pivot's.
        let mut nw = 0;
        for &(c, cands) in bivalues {
            if sees(pivot, c) && cands.without(pcands).is_empty() {
                wingbuf[nw] = (c, cands);
                nw += 1;
            }
        }
        if nw < 2 {
            continue;
        }
        let wings = &wingbuf[..nw];
        let mut fired = false;
        for_each_combination(wings, 2, |combo| {
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

/// Per-digit **conjugate pairs** of a board: for each digit `d`, the units (walked in
/// [`UNITS`] order) that hold `d` in exactly two cells, recorded as that cell pair —
/// the raw material of the W-Wing's strong link. The board is constant across a single
/// [`w_wing`] scan (it eliminates only on the firing step, then returns), so the table
/// is built once and every [`w_wing_link`] attempt reads it instead of rescanning all
/// 27 units per bivalue pair x link digit. Unit order is preserved, so the first firing
/// link — and thus the verdict — is byte-identical to the per-attempt rescan.
struct ConjugatePairs {
    /// `pairs[d][..len[d]]`: digit `d`'s conjugate cell pairs, in unit order. At most
    /// one per unit, so 27 bounds each digit's list.
    pairs: [[(CellIdx, CellIdx); 27]; 9],
    len: [u8; 9],
}

impl ConjugatePairs {
    /// One pass over the 27 units: per unit, transpose the nine cell masks into per-digit
    /// slot-position masks, then emit a conjugate pair for every digit held by exactly two
    /// cells (popcount 2). The transpose is **branchless** — `pos[di]` bit `i` is set to
    /// bit `di` of slot `i`'s mark — the same shape the subset cache's hidden transpose
    /// uses, and for the same reason: a per-cell `trailing_zeros` scatter with a per-digit
    /// `match` is data-dependent-branch heavy and measurably mispredicts, where the fixed
    /// 9-wide transpose carries no such branch. Cells are walked in slot order and the two
    /// set bits read low-first, so each pair stays `(lower-slot, higher-slot)` and the
    /// per-digit lists stay in unit order — byte-identical to the per-digit tally it
    /// replaces (and thus to the verdict and generator fingerprint).
    fn scan<V: LogicBoard>(v: &V) -> Self {
        let mut pairs = [[(0usize, 0usize); 27]; 9];
        let mut len = [0u8; 9];
        for unit in &UNITS {
            // pos[di] = 9-bit mask (over unit slots) of the cells holding digit `di`.
            let mut pos = [0u16; 9];
            for i in 0..9 {
                let row = v.get(unit[i]).bits();
                for di in 0..9 {
                    pos[di] |= ((row >> di) & 1) << i;
                }
            }
            for di in 0..9 {
                if pos[di].count_ones() == 2 {
                    let s0 = pos[di].trailing_zeros() as usize;
                    let s1 = (pos[di] & (pos[di] - 1)).trailing_zeros() as usize;
                    pairs[di][len[di] as usize] = (unit[s0], unit[s1]);
                    len[di] += 1;
                }
            }
        }
        Self { pairs, len }
    }

    #[inline]
    fn for_digit(&self, d: Digit) -> &[(CellIdx, CellIdx)] {
        let di = d.index();
        &self.pairs[di][..self.len[di] as usize]
    }
}

/// **W-Wing**: two bivalue cells `X`, `Y` with the *same* candidates `{P,Q}` that are
/// not peers, joined by a conjugate pair on one digit (say `P`) in some unit — its
/// two cells seeing `X` and `Y` respectively. Then `Q` leaves every cell that sees
/// both `X` and `Y`. Ported from core's `w_wing::find_each` + `try_link`.
///
/// The same-pair cells are exactly one [`BivalueBuckets`] bucket, so the pair search
/// walks each non-trivial bucket instead of testing `xcands == ycands` over all O(n²)
/// bivalue pairs. The strong-link search needs the board's conjugate pairs
/// ([`ConjugatePairs`]); they are built lazily on the first non-peer same-pair pair, so
/// a board with no such pair pays nothing.
fn w_wing<V: LogicBoard>(v: &mut V, buckets: &BivalueBuckets) -> bool {
    let mut conj: Option<ConjugatePairs> = None;
    for slot in 0..36 {
        let bucket = buckets.run(slot);
        if bucket.len() < 2 {
            continue;
        }
        // The bucket's shared pair {P,Q} (ascending), the two strong-link candidates.
        let mask = SLOT_TO_MASK[slot];
        let d0 = Digit::from_index(mask.trailing_zeros() as usize);
        let d1 = Digit::from_index((mask & (mask - 1)).trailing_zeros() as usize);
        for (i, &x) in bucket.iter().enumerate() {
            for &y in &bucket[i + 1..] {
                if sees(x, y) {
                    continue;
                }
                let conj = conj.get_or_insert_with(|| ConjugatePairs::scan(v));
                // Try each shared digit as the strong-link digit; the other is eliminated
                // (ascending digit order, as before).
                if w_wing_link(v, conj, x, y, d0, d1) || w_wing_link(v, conj, x, y, d1, d0) {
                    return true;
                }
            }
        }
    }
    false
}

/// One W-Wing strong-link attempt: among the precomputed conjugate pairs on `link`
/// (in unit order), find one whose endpoints see `x` and `y` respectively; if found,
/// `other` leaves every cell seeing both `x` and `y`. Returns whether it eliminated
/// anything (first firing pair wins).
fn w_wing_link<V: LogicBoard>(
    v: &mut V,
    conj: &ConjugatePairs,
    x: CellIdx,
    y: CellIdx,
    link: Digit,
    other: Digit,
) -> bool {
    for &(c1, c2) in conj.for_digit(link) {
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
