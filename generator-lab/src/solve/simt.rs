//! Packed SoA **baseline logic solver** on the `repr` layer: the `solve` analogue of
//! [`probe::simt`](crate::probe::simt). Where the packed prober batches the strip
//! loop's *uniqueness* gates across W=8 SIMD lanes, this batches its *baseline* gates
//! — the technique-driven, non-backtracking [`FusedLogicSolver`](super::FusedLogicSolver)
//! the SIMT codepath still runs per lane (the remaining scalar half of the warp).
//!
//! ## The cheap-closure / subset split (data-driven)
//!
//! Measured on the production `train`/`drill(HiddenQuad)` specs (`examples/baselinestat`),
//! the baseline gate's work splits sharply: the **cheap closure** (naked + hidden
//! singles, locked candidates) SOLVES ~97% of calls in ~1 fixpoint pass, and only ~2%
//! ever reach the combinatorial **subset ladder** (naked/hidden pair..quad). So this
//! solver **vectorizes the cheap closure** ([`warp_pass_full`], lane-uniform band ALU)
//! and keeps the rare subset step **scalar per-lane** ([`subset_step`], reusing the
//! composable [`super::techniques`] on a snapshot) — exactly the way the prober keeps
//! its branch-cell pick and frame snapshot scalar.
//!
//! ## Gather-free, single-view
//!
//! Like the prober, the closure runs on a **single row-major** SoA board (the prober's
//! [`smear_v`]/[`one_bit`] geometry, shared verbatim). A logic solver must not
//! under-solve, so unlike the prober it cannot skip columns — but rather than carry a
//! second column-major view (and transpose every placement), column hidden singles are
//! found by a **column fold + broadcast** straight off the row bands ([`warp_pass_full`]),
//! with no gather and no transpose.
//!
//! ## Where locked candidates run (measured)
//!
//! LC has two implementations, selectable via [`PackedSolver::run_stream_with`]:
//!   - **off the warp (default):** the closure does only singles; a stalled lane runs
//!     the fast table-driven scalar LC fixpoint ([`scalar_lc_fast`], the same `DROP_TRIP`
//!     LC `FusedLogicSolver` uses) before the subsets, then rejoins.
//!   - **in the closure (research):** a vectorized single LC round per pass
//!     ([`lc_eliminate`], gather-free), the outer loop reaching the fixpoint.
//!
//! Off-warp wins on the bench (`baselinebench`): train isolated **1.87x** vs **1.36x**
//! for in-closure. The closure's LC taxes every lane every pass, but ~70% of calls never
//! need LC (singles solve them), whereas off-warp only pays LC on the ~30% that stall.
//! The in-closure path is kept (behind the knob, parity-tested) for future work — the
//! gap is narrow and the vectorized round may yet be cheapened.
//!
//! Verdicts (`solved` + the subset-kind counts the spec's requirement check reads) are
//! identical to [`FusedLogicSolver`](super::FusedLogicSolver), so the solver is a
//! drop-in for the warp's baseline gate. Cheap-kind counts are not tracked (the fused
//! contract leaves them undefined, and the fast path's requirement check never reads
//! one — a Forced cheap kind routes off the fused/SIMT path entirely).

use super::combinations::for_each_combination;
use super::techniques;
use crate::counters::counter_block;
use crate::probe::simt::{
    BOX_CELLS, Frame, LANES, M, ONE, Probe, ROW_MASK, V, ZERO, assign, backtrack_lane, branch_lane,
    load_lane, one_bit, restore_lane, rm_cell, smear_v, snapshot_lane, warp_pass,
};
use crate::repr::banded::{Bands, RowMajor};
use crate::repr::{CellIdx, Digit, DigitGrid, Mark, Marks, Occupancy, SolverState, UNITS};
use crate::solve::Eliminate;
use crate::spec::kinds::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_TRIPLE, KindMask, LC_CLAIMING, LC_POINTING, NAKED_PAIR,
    NAKED_QUAD, NAKED_TRIPLE, NUM, SolveTrace,
};
use std::simd::cmp::SimdPartialEq;
use std::simd::num::SimdUint;
use std::simd::{Select, Simd};

// --- packed baseline-solver utilization (feature = "count") -------------------
// [0] warp passes (ticks), [1] active-lane-sum over ticks. utilization =
// active_lane_sum / (LANES * ticks) — the baseline warp's analogue of the prober's
// DSTAT[3]/(LANES*DSTAT[1]). Read by the `simtutil` example to size the buffer.
counter_block!(SSTAT: 2, inc = sstat_inc, add = sstat_add, snapshot = sstat_snapshot, reset = sstat_reset);

// --- unified-warp utilization (feature = "count") -----------------------------
// The single warp that runs probe AND baseline lanes together ([`UnifiedWarp`]):
// [0] warp passes, [1] active-lane-sum over passes. util = [1]/(LANES*[0]). Unlike
// the two-warp hosts (separate probe DSTAT / baseline SSTAT), this is ONE number —
// the whole point of unifying is that the combined warp is full whenever either kind
// of work exists, so no oversubscription is needed to fill it. Read by `simtutil`.
counter_block!(UWSTAT: 2, inc = uwstat_inc, add = uwstat_add, snapshot = uwstat_snapshot, reset = uwstat_reset);

/// One lane's input to the packed baseline solver: its row-major per-digit candidate
/// bands and empty mask — the same row view ([`SolverState<Bands<RowMajor>>`] via
/// [`Bands::to_lanes`](crate::repr::banded)) the strip hands the prober, minus the
/// prober-only `cell`/`alts` restriction (the baseline solves the board as stripped).
#[derive(Clone, Copy)]
pub struct SolveQuery {
    pub r: [[u32; 4]; 9],
    pub unsolved: [u32; 4],
}

impl SolveQuery {
    /// An inert padding query for filling a fixed scratch array beyond the live count.
    pub const EMPTY: SolveQuery = SolveQuery { r: [[0; 4]; 9], unsolved: [0; 4] };

    /// The row-major SoA query of a digit grid — the `from_digits` baseline board, for
    /// the isolated bench and the parity test (the warp host instead exports the
    /// incrementally-maintained strip board directly).
    pub fn from_digits(grid: &DigitGrid) -> Self {
        let st = SolverState::<Bands<RowMajor>>::from_digits(grid);
        let r = core::array::from_fn(|d| st.candidates().each()[d].to_lanes());
        SolveQuery { r, unsolved: st.unsolved().to_lanes() }
    }
}

/// Load query `q` into lane `l` of the SoA boards — the per-lane scalar store of a
/// fresh baseline board (the refill path).
#[inline]
fn load_query(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], l: usize, q: &SolveQuery) {
    for d in 0..9 {
        for b in 0..3 {
            r[d][b].as_mut_array()[l] = q.r[d][b];
        }
    }
    for b in 0..3 {
        unsolved[b].as_mut_array()[l] = q.unsolved[b];
    }
}

/// ONE cheap-closure pass across the warp, applied only to `active` lanes: naked
/// singles + hidden singles in **every** unit (rows, boxes, AND columns) fused into one
/// per-digit placement sweep. Returns per-lane `(changed, dead, solved)`; the scheduler
/// iterates until each lane is solved / dead / stuck. The naked-single sieve and the
/// row/box hidden-single detection + [`smear_v`] placement are the prober's
/// [`warp_pass`](crate::probe::simt) verbatim; the **column** hidden singles are the
/// one addition a non-backtracking solver needs (the prober reaches columns by
/// branching, so its pass omits them).
///
/// **Columns, gather-free.** A column `c` (0..9) holds its candidate cells at bit
/// positions `c`, `c+9`, `c+18` of each of the three bands. Folding those nine 9-bit
/// row-slices of digit `d`'s live board into per-column "seen once" / "seen twice"
/// gives the columns with exactly one live candidate — a hidden single. Broadcasting
/// that 9-bit column mask back across the three rows (`m | m<<9 | m<<18`) and AND-ing
/// the live board picks the single forced cell, in whichever band/row it sits, with no
/// transpose and no column-major view.
///
/// `LC` (a const, monomorphized away for drill, whose baseline has no locked
/// candidates) adds one round of pointing+claiming per pass ([`lc_eliminate`]); the
/// outer warp loop iterates the closure, so a single round per pass reaches the same
/// within-band LC fixpoint the scalar table reaches in one shot — gather-free.
#[cfg_attr(feature = "profiling", inline(never))]
fn warp_pass_full<const LC: bool>(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], active: M) -> (M, M, M) {
    let mut changed = M::splat(false);
    let mut dead = M::splat(false);

    // Naked singles: ones = "at least one candidate", twos = "at least two", per band,
    // accumulated across the nine digit boards — lane-parallel (prober's sieve).
    let mut ones = [ZERO; 3];
    let mut twos = [ZERO; 3];
    for d in 0..9 {
        for b in 0..3 {
            let x = r[d][b];
            twos[b] |= ones[b] & x;
            ones[b] |= x;
        }
    }
    let mut singles = [ZERO; 3];
    for b in 0..3 {
        dead |= (unsolved[b] & !ones[b]).simd_ne(ZERO); // an unsolved cell with no candidate
        singles[b] = unsolved[b] & ones[b] & !twos[b];
    }

    let m9: V = Simd::splat(0x1FF);
    let nine: V = Simd::splat(9);
    let eighteen: V = Simd::splat(18);
    for d in 0..9 {
        let mut group = [singles[0] & r[d][0], singles[1] & r[d][1], singles[2] & r[d][2]];
        // Row + box hidden singles (each unit a contiguous / box-gathered 9-bit run, in
        // lane in the row banding).
        for b in 0..3 {
            let live = r[d][b] & unsolved[b];
            for rr in 0..3 {
                let rc = live & Simd::splat(ROW_MASK[rr]);
                group[b] |= one_bit(rc).select(rc, ZERO);
            }
            for bx in 0..3 {
                let bc = live & Simd::splat(BOX_CELLS[bx]);
                group[b] |= one_bit(bc).select(bc, ZERO);
            }
        }
        // Column hidden singles via the column fold + broadcast (see fn docs).
        let mut cones = ZERO;
        let mut ctwos = ZERO;
        for b in 0..3 {
            let live = r[d][b] & unsolved[b];
            for rr in 0..3u32 {
                let slice = (live >> Simd::splat(9 * rr)) & m9;
                ctwos |= cones & slice;
                cones |= slice;
            }
        }
        let col_single = cones & !ctwos;
        let col_bc = col_single | (col_single << nine) | (col_single << eighteen);
        for b in 0..3 {
            group[b] |= (r[d][b] & unsolved[b]) & col_bc;
        }

        let (peers, conflict) = smear_v(group);
        dead |= conflict;
        for b in 0..3 {
            let gm = active.select(group[b], ZERO);
            unsolved[b] &= !gm;
            r[d][b] &= !active.select(peers[b], ZERO);
            changed |= gm.simd_ne(ZERO);
        }
    }

    // Locked candidates (one round; the outer loop reaches the fixpoint). Eliminations
    // on the post-placement board; any LC-induced dead cell is caught next pass by the
    // naked-single sieve (`unsolved & !ones`), exactly as the scalar engine defers it to
    // the next propagate.
    if LC {
        changed |= lc_eliminate(r, unsolved, active);
    }

    dead &= active;
    changed &= active;
    let empties = unsolved[0].count_ones() + unsolved[1].count_ones() + unsolved[2].count_ones();
    let solved = active & empties.simd_eq(ZERO) & !dead;
    (changed, dead, solved)
}

/// A band's 9-bit triplet occupancy off its live candidates: bit `t` (`t = 3*row +
/// boxcol`) set iff the triplet's three cells `[3t, 3t+3)` hold any candidate.
#[inline]
fn triplet_occ(live: V) -> V {
    let mut occ = ZERO;
    for t in 0..9u32 {
        let tm = Simd::splat(0b111u32 << (3 * t));
        occ |= (live & tm).simd_ne(ZERO).select(Simd::splat(1u32 << t), ZERO);
    }
    occ
}

/// ONE round of within-band locked candidates (pointing + claiming) over a 9-bit
/// triplet occupancy `occ` (bit `3*row + col`): the occupied triplets it eliminates.
/// Pointing — a box-column whose occupied triplets lie in a single band-row clears that
/// row's other columns; claiming — a band-row whose occupied triplets lie in a single
/// box-column clears that column's other rows. Reads the same `occ` throughout (a
/// single round, all-at-once); the outer warp loop re-runs the closure, and the
/// clearing is monotone, so the global fixpoint matches the scalar table's. Vectorized
/// ALU, no `DROP_TRIP` gather.
#[inline]
fn lc_round_drop(occ: V) -> V {
    let mut dropped = ZERO;
    // Pointing: for each box-column k, if exactly one band-row is occupied, drop that
    // row's other two columns.
    for k in 0..3u32 {
        let r0 = (occ >> Simd::splat(k)) & ONE;
        let r1 = (occ >> Simd::splat(3 + k)) & ONE;
        let r2 = (occ >> Simd::splat(6 + k)) & ONE;
        let single = (r0 + r1 + r2).simd_eq(ONE);
        for rr in 0..3u32 {
            let row_has = ((occ >> Simd::splat(3 * rr + k)) & ONE).simd_eq(ONE);
            let active = single & row_has; // band-row rr is the single occupied row
            let dropmask = (0b111u32 << (3 * rr)) & !(1u32 << (3 * rr + k));
            dropped |= active.select(Simd::splat(dropmask) & occ, ZERO);
        }
    }
    // Claiming: for each band-row r, if exactly one box-column is occupied, drop that
    // column's other two rows.
    for r in 0..3u32 {
        let c0 = (occ >> Simd::splat(3 * r)) & ONE;
        let c1 = (occ >> Simd::splat(3 * r + 1)) & ONE;
        let c2 = (occ >> Simd::splat(3 * r + 2)) & ONE;
        let single = (c0 + c1 + c2).simd_eq(ONE);
        for kk in 0..3u32 {
            let col_has = ((occ >> Simd::splat(3 * r + kk)) & ONE).simd_eq(ONE);
            let active = single & col_has;
            let dropmask = ((1u32 << kk) | (1 << (3 + kk)) | (1 << (6 + kk))) & !(1u32 << (3 * r + kk));
            dropped |= active.select(Simd::splat(dropmask) & occ, ZERO);
        }
    }
    dropped
}

/// Expand a 9-bit dropped-triplet mask to the 27-bit cell mask it clears (each triplet
/// `t` -> its three cells `0b111 << 3t`).
#[inline]
fn expand_triplets(dropped: V) -> V {
    let mut clear = ZERO;
    for t in 0..9u32 {
        let has = ((dropped >> Simd::splat(t)) & ONE).simd_eq(ONE);
        clear |= has.select(Simd::splat(0b111u32 << (3 * t)), ZERO);
    }
    clear
}

/// One round of locked candidates across the warp, both orientations, applied to
/// `active` lanes — returns the lanes that changed. Box↔row LC is in-lane per band
/// ([`triplet_occ`] + [`lc_round_drop`] + [`expand_triplets`]); box↔column LC reads the
/// column-major triplet occupancy folded across the three row bands (no second view),
/// and its eliminations clear a column-segment in the triplet's band.
#[inline]
fn lc_eliminate(r: &mut [[V; 3]; 9], unsolved: &[V; 3], active: M) -> M {
    let mut changed = M::splat(false);

    // Box <-> row: each band's three boxes against its three rows, all in-lane.
    for b in 0..3 {
        for d in 0..9 {
            let live = r[d][b] & unsolved[b];
            let dropped = lc_round_drop(triplet_occ(live));
            let clear = expand_triplets(dropped);
            r[d][b] &= !active.select(clear, ZERO);
            changed |= active & dropped.simd_ne(ZERO);
        }
    }

    // Box <-> column: each stack's three boxes against its three columns. The
    // column-major triplet (a = column-in-stack, g = band) occupies bit `3a + g`, read
    // from the three row bands; its elimination clears column `3s + a`'s three cells in
    // band `g`.
    for s in 0..3u32 {
        for d in 0..9 {
            let mut occ = ZERO;
            for a in 0..3u32 {
                let c = 3 * s + a;
                let cm: V = Simd::splat((1u32 << c) | (1 << (c + 9)) | (1 << (c + 18)));
                for gi in 0..3usize {
                    let live = r[d][gi] & unsolved[gi];
                    occ |= (live & cm)
                        .simd_ne(ZERO)
                        .select(Simd::splat(1u32 << (3 * a + gi as u32)), ZERO);
                }
            }
            let dropped = lc_round_drop(occ);
            for t in 0..9u32 {
                let has = ((dropped >> Simd::splat(t)) & ONE).simd_eq(ONE);
                let a = t / 3;
                let g = (t % 3) as usize;
                let c = 3 * s + a;
                let cm: V = Simd::splat((1u32 << c) | (1 << (c + 9)) | (1 << (c + 18)));
                r[d][g] &= !(active & has).select(cm, ZERO);
            }
            changed |= active & dropped.simd_ne(ZERO);
        }
    }

    changed
}

/// The discrete "harder than the cheap closure" ladder, gated by `allowed` — the
/// scalar per-lane fallback the warp drops a stalled lane to. Mirror of
/// [`FusedLogicSolver`](super::FusedLogicSolver)'s `step_harder`: the cheap closure
/// has already drained singles + LC, so only the subsets (NakedPair..HiddenQuad), the
/// basic fish (X-Wing..Jellyfish), and the bivalue wings (XY-/XYZ-/W-Wing) remain.
/// Returns the kind index of the first technique that fired, or `None`. The try-order
/// is this engine's choice (follows core's, un-optimized); branch-scoped specs never
/// have two Expert branches in scope together, so their relative order is moot in
/// production.
///
/// The six subsets share a single [`SubsetCache`] built up front (so the per-unit
/// transpose is paid once, not three times per kind); the rarer fish/wings — guarded
/// out entirely for the subset-only HiddenQuad toolbox — fall back to the generic
/// [`super::techniques`] bodies on the cell-major `cm` (every unit reachable via `get`,
/// columns included).
fn cellmarks_step_harder(cm: &mut CellMarks, allowed: KindMask) -> Option<usize> {
    const ANY_SUBSET: KindMask = (1 << NAKED_PAIR)
        | (1 << HIDDEN_PAIR)
        | (1 << NAKED_TRIPLE)
        | (1 << HIDDEN_TRIPLE)
        | (1 << NAKED_QUAD)
        | (1 << HIDDEN_QUAD);
    // The cache only feeds the subset ladder; a subset-free toolbox (e.g. drill, whose
    // baseline is singles-only) skips the build entirely and falls straight through.
    if allowed & ANY_SUBSET != 0 {
        let cache = SubsetCache::build(cm);
        macro_rules! try_kind {
            ($bit:expr, $call:expr) => {
                if allowed & (1 << $bit) != 0 && $call {
                    return Some($bit);
                }
            };
        }
        try_kind!(NAKED_PAIR, cached_naked_subset(cm, &cache, 2));
        try_kind!(HIDDEN_PAIR, cached_hidden_subset(cm, &cache, 2));
        try_kind!(NAKED_TRIPLE, cached_naked_subset(cm, &cache, 3));
        try_kind!(HIDDEN_TRIPLE, cached_hidden_subset(cm, &cache, 3));
        try_kind!(NAKED_QUAD, cached_naked_subset(cm, &cache, 4));
        try_kind!(HIDDEN_QUAD, cached_hidden_subset(cm, &cache, 4));
    }
    if let Some(k) = techniques::fish_step(cm, allowed) {
        return Some(k);
    }
    if let Some(k) = techniques::wing_step(cm, allowed) {
        return Some(k);
    }
    None
}

/// Per-unit candidate cache for the subset ladder, built ONCE per stall and shared by
/// all six subset techniques. The generic [`techniques::naked_subset`] /
/// [`techniques::hidden_subset`] each re-derive their per-unit inputs on every call, so
/// the three sizes rebuild the same per-unit marks (naked) and the same digit-position
/// transpose (hidden) three times over. Here the difficulty order is unchanged — each
/// size still scans all 27 units, first-fire-wins — but the inputs are read from this
/// cache, so the transpose is paid once. Valid because the board is untouched until a
/// technique fires (and then [`cellmarks_step_harder`] returns), so a cache built before
/// the first scan stays exact for every later size.
struct SubsetCache {
    /// `marks[u][i]` = the candidate set of unit `u`'s `i`-th cell (`UNITS[u][i]`).
    marks: [[Mark; 9]; 27],
    /// `positions[u][d]` = the 9-bit mask (over unit slots) of cells in unit `u` where
    /// digit `d` is a candidate — the hidden-subset transpose of `marks[u]`.
    positions: [[u16; 9]; 27],
}

impl SubsetCache {
    /// Build both views off the cell-major [`CellMarks`] in one pass per unit: read each
    /// cell's mark (naked's input) and transpose it into the per-digit position masks
    /// (hidden's input). The transpose is **branchless** — `positions[di]` bit `i` is set
    /// to bit `di` of slot `i`'s mark — so it adds no data-dependent branch (the closure
    /// is mispredict-bound; a `trailing_zeros` scatter loop here measurably raised
    /// branch-misses). Paid once, it replaces the generic hidden body's per-size `contains`
    /// sweep run three times over.
    fn build(cm: &CellMarks) -> Self {
        let mut marks = [[Mark::EMPTY; 9]; 27];
        let mut positions = [[0u16; 9]; 27];
        for u in 0..27 {
            for i in 0..9 {
                let mk = cm.marks[UNITS[u][i]];
                marks[u][i] = mk;
                let row = mk.bits();
                for di in 0..9 {
                    positions[u][di] |= ((row >> di) & 1) << i;
                }
            }
        }
        SubsetCache { marks, positions }
    }
}

/// Cache-fed [`techniques::naked_subset`] (see [`SubsetCache`]): identical first-fire
/// logic and elimination order, reading the precomputed per-unit marks instead of
/// re-gathering them per size. Eliminations are logged into `cm` (only ever on the
/// firing step, after which the ladder returns).
fn cached_naked_subset(cm: &mut CellMarks, cache: &SubsetCache, size: usize) -> bool {
    for u in 0..27 {
        let marks = &cache.marks[u];
        // Candidate slots: empty cells with 2..=size candidates (a filled cell reads as
        // the empty mark, so `len` 0 excludes it). `cand` holds slot indices 0..9.
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
                    cm.eliminate(UNITS[u][i], d);
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

/// Cache-fed [`techniques::hidden_subset`] (see [`SubsetCache`]): identical first-fire
/// logic and elimination order, reading the precomputed per-unit position masks instead
/// of rebuilding the digit-position transpose per size.
fn cached_hidden_subset(cm: &mut CellMarks, cache: &SubsetCache, size: usize) -> bool {
    for u in 0..27 {
        let marks = &cache.marks[u];
        let pos_all = &cache.positions[u];
        // Digits with 2..=size candidate cells in this unit (a placed digit has none).
        let mut digits = [0usize; 9];
        let mut n = 0;
        for di in 0..9 {
            let pc = pos_all[di].count_ones() as usize;
            if (2..=size).contains(&pc) {
                digits[n] = di;
                n += 1;
            }
        }
        if n < size {
            continue;
        }
        let mut applied = false;
        for_each_combination(&digits[..n], size, |combo| {
            let union: u16 = combo.iter().map(|&di| pos_all[di]).fold(0, |a, x| a | x);
            if union.count_ones() as usize != size {
                return true;
            }
            // The combo digits stay; every other candidate leaves the union's cells.
            let keep = combo.iter().fold(Mark::EMPTY, |mut acc, &di| {
                acc.insert(Digit::from_index(di));
                acc
            });
            let mut did = false;
            for i in 0..9 {
                if union & (1 << i) == 0 {
                    continue;
                }
                for d in marks[i].without(keep).iter() {
                    cm.eliminate(UNITS[u][i], d);
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

/// Snapshot stalled lane `l` out of the warp into a scalar [`SolverState`], run one
/// [`scalar_step_harder`] on it, and (if it fired) write the pruned board back into
/// the lane so the next [`warp_pass_full`] resumes the cheap closure. Returns the kind
/// that fired, or `None` when nothing applies (the lane is unsolvable under the
/// toolbox). Snapshot/restore is the prober's per-branch clone path; the harder search
/// is the rare ~2% tail, so paying it per-lane scalar is cheap.
fn subset_step(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], l: usize, allowed: KindMask, try_lc: bool) -> Option<usize> {
    let (mut sr, su) = snapshot_lane(r, unsolved, l);
    // LC-off-warp: run the FAST (table-driven) scalar LC fixpoint on the snapshot before
    // the subsets; if it pruned anything, rejoin the warp so the vectorized singles
    // resume. This is the fair off-warp comparison — same `DROP_TRIP` LC the scalar
    // `FusedLogicSolver` uses, not the slow composable one.
    if try_lc && scalar_lc_fast(&mut sr, &su) {
        restore_lane(r, unsolved, l, &sr, &su);
        return Some(LC_POINTING); // a cheap kind: counted-but-never-read, signals "progress"
    }
    // The subset ladder runs all six (naked/hidden x pair/triple/quad) techniques in
    // difficulty order, each re-scanning every unit's cells through `get`. On the
    // digit-major `SolverState<Bands>` `get` is a 9-board scan; transposing the snapshot
    // once into the cell-major [`CellMarks`] makes every `get` an O(1) `Mark` load — the
    // same fast surface `verify` uses (`Board`). Subsets only prune (never place), so the
    // empty mask is unchanged and only the candidate bands are read back.
    let mut cm = CellMarks::from_bands(&sr, &su);
    let k = cellmarks_step_harder(&mut cm, allowed)?;
    // A fired subset removes only a handful of candidates, so write them straight into the
    // warp lane (one bit clear each) rather than rebuilding + scattering the whole board.
    // `r` is untouched since the snapshot (the LC fixpoint runs on the local copy `sr`), so
    // clearing the logged bits leaves the lane exactly as a full restore would — and the
    // empty mask is unchanged (subsets only prune), so `unsolved` needs no write at all.
    for &(cell, d) in &cm.elims[..cm.n_elim] {
        let (b, bit) = cell_band_bit(cell as usize);
        r[d as usize][b].as_mut_array()[l] &= !(1u32 << bit);
    }
    Some(k)
}

/// Candidate-only **cell-major** board for the scalar subset step (see [`subset_step`]):
/// each cell's [`Mark`] stored directly, so a technique's per-unit scan reads it in O(1)
/// rather than the digit-major snapshot's 9-board scan per `get`. The cell<->(band, bit)
/// map is [`RowMajor`]'s (`band = cell / 27`, `bit = (cell / 9 % 3) * 9 + cell % 9`).
/// Candidates only: the subset ladder never places, so `unsolved` is carried solely for
/// [`Occupancy`] and the unused `Marks::from_digits`/`place` are not meaningful.
#[derive(Clone)]
struct CellMarks {
    marks: [Mark; 81],
    /// Row-major empty mask (the snapshot's `su`), the one fact candidates can't carry.
    unsolved: [u32; 3],
    /// Log of `(cell, digit-index)` candidates removed by [`eliminate`](CellMarks::eliminate),
    /// so [`subset_step`] writes back only what the fired technique pruned (a targeted bit
    /// clear per entry) instead of rebuilding + scattering the whole lane. One firing of any
    /// harder technique — a subset over one nine-cell unit, or a basic fish of `size` cover
    /// lines — removes at most `size * (9 - size) <= 20` candidates, so the fixed buffer never
    /// overflows (asserted).
    elims: [(u8, u8); 32],
    n_elim: usize,
}

/// Cell `c`'s `(band, bit)` in the [`RowMajor`] packing.
#[inline]
fn cell_band_bit(c: usize) -> (usize, u32) {
    (c / 27, (((c / 9) % 3) * 9 + c % 9) as u32)
}

impl CellMarks {
    /// Transpose a stalled lane's snapshot bands into per-cell marks (empty cells only;
    /// a solved cell reads as [`Mark::EMPTY`], matching `SolverState::get`).
    fn from_bands(sr: &[[u32; 3]; 9], su: &[u32; 3]) -> Self {
        let mut marks = [Mark::EMPTY; 81];
        for (c, slot) in marks.iter_mut().enumerate() {
            let (b, bit) = cell_band_bit(c);
            if (su[b] >> bit) & 1 == 0 {
                continue; // solved -> EMPTY
            }
            let mut m = Mark::EMPTY;
            for (d, srd) in sr.iter().enumerate() {
                if (srd[b] >> bit) & 1 != 0 {
                    m.insert(Digit::from_index(d));
                }
            }
            *slot = m;
        }
        CellMarks { marks, unsolved: *su, elims: [(0, 0); 32], n_elim: 0 }
    }
}

impl Marks for CellMarks {
    fn from_digits(_: &DigitGrid) -> Self {
        unreachable!("CellMarks is built from snapshot bands, not a digit grid")
    }
    fn place(&mut self, _: CellIdx, _: Digit) {
        unreachable!("the subset ladder never places, only eliminates")
    }
    #[inline]
    fn get(&self, cell: CellIdx) -> Mark {
        self.marks[cell]
    }
}

impl Occupancy for CellMarks {
    #[inline]
    fn is_empty(&self, cell: CellIdx) -> bool {
        let (b, bit) = cell_band_bit(cell);
        (self.unsolved[b] >> bit) & 1 != 0
    }
}

impl Eliminate for CellMarks {
    #[inline]
    fn eliminate(&mut self, cell: CellIdx, d: Digit) {
        debug_assert!(self.n_elim < self.elims.len(), "subset eliminations exceeded buffer");
        self.elims[self.n_elim] = (cell as u8, d.index() as u8);
        self.n_elim += 1;
        self.marks[cell].remove(d);
    }
}

/// The fast scalar locked-candidates fixpoint over a snapshot's row bands — the
/// table-driven (`DROP_TRIP`) LC the scalar `FusedLogicSolver` uses, here on a single
/// lane's snapshot for the LC-off-warp experiment. Box↔row is in-lane per band;
/// box↔column reads the column-major triplet occupancy folded across the three bands
/// (same geometry as the vectorized [`lc_eliminate`]) and clears a column-segment in the
/// triplet's band. Loops over both orientations until a full sweep changes nothing.
/// Returns whether anything was eliminated. LC only prunes candidates, so `unsolved` is
/// untouched.
fn scalar_lc_fast(sr: &mut [[u32; 3]; 9], su: &[u32; 3]) -> bool {
    use crate::solve::fused::DROP_TRIP;
    let occ_row = |live: u32| -> usize {
        let mut o = 0usize;
        for t in 0..9 {
            if live & (0b111u32 << (3 * t)) != 0 {
                o |= 1 << t;
            }
        }
        o
    };
    let mut any = false;
    loop {
        let mut changed = false;
        // Box <-> row, in-lane per band.
        for b in 0..3 {
            for d in 0..9 {
                let live = sr[d][b] & su[b];
                let dropped = DROP_TRIP[occ_row(live)];
                if dropped != 0 {
                    let mut clear = 0u32;
                    let mut dd = dropped;
                    while dd != 0 {
                        let t = dd.trailing_zeros();
                        dd &= dd - 1;
                        clear |= 0b111u32 << (3 * t);
                    }
                    let before = sr[d][b];
                    sr[d][b] &= !clear;
                    changed |= sr[d][b] != before;
                }
            }
        }
        // Box <-> column: stack s, triplet (a = col-in-stack, g = band) at bit 3a+g.
        for s in 0..3u32 {
            for d in 0..9 {
                let mut occ = 0usize;
                for a in 0..3u32 {
                    let c = 3 * s + a;
                    let cm = (1u32 << c) | (1 << (c + 9)) | (1 << (c + 18));
                    for gi in 0..3usize {
                        if (sr[d][gi] & su[gi]) & cm != 0 {
                            occ |= 1 << (3 * a as usize + gi);
                        }
                    }
                }
                let mut dd = DROP_TRIP[occ];
                while dd != 0 {
                    let t = dd.trailing_zeros();
                    dd &= dd - 1;
                    let a = t / 3;
                    let g = (t % 3) as usize;
                    let c = 3 * s + a;
                    let cm = (1u32 << c) | (1 << (c + 9)) | (1 << (c + 18));
                    let before = sr[d][g];
                    sr[d][g] &= !cm;
                    changed |= sr[d][g] != before;
                }
            }
        }
        if changed {
            any = true;
        } else {
            break;
        }
    }
    any
}

/// Scalar column-hidden-single recovery for one stalled baseline lane under the LEAN
/// kernel (the cheap [`warp_pass`] omits columns). Detects lane `l`'s column hidden
/// singles off its extracted row bands — column `c` holds digit `d`'s candidates at bits
/// `c, c+9, c+18` of each band; fold the nine row-slices into per-column seen-once /
/// seen-twice and `c` is forced iff exactly one — and [`assign`]s each (restricts the
/// cell to that one digit). The next [`warp_pass`] then places it as a naked single via
/// the smear, so NO scalar peer-clear is needed. Every assign is sound (the column leaves
/// the digit exactly one home), so the closure stays confluent and the verdict is
/// unchanged; `assign` clearing the cell's other digits also removes it from later digits'
/// folds in the same scan, so the per-digit sequence matches `warp_pass_full`'s column
/// block. Returns whether anything was assigned (then the lane rejoins the warp). This is
/// the gather-free, full-pass-free way to keep columns off the probe lanes (the masked
/// SIMD recovery it replaces paid the column ALU warp-wide regardless of the mask).
fn scalar_col_assign(r: &mut [[V; 3]; 9], unsolved: &[V; 3], l: usize) -> bool {
    let u = [unsolved[0].as_array()[l], unsolved[1].as_array()[l], unsolved[2].as_array()[l]];
    let mut assigned = false;
    for d in 0..9 {
        let mut cones = 0u32;
        let mut ctwos = 0u32;
        for b in 0..3 {
            let live = r[d][b].as_array()[l] & u[b];
            for rr in 0..3u32 {
                let slice = (live >> (9 * rr)) & 0x1FF;
                ctwos |= cones & slice;
                cones |= slice;
            }
        }
        let mut col_single = cones & !ctwos;
        while col_single != 0 {
            let c = col_single.trailing_zeros();
            col_single &= col_single - 1;
            // Locate the forced cell: the (band, row) whose digit-`d` live candidate sits
            // in column `c` (bit `9*rr + c`), then assign `d` there.
            'find: for b in 0..3 {
                let live = r[d][b].as_array()[l] & u[b];
                for rr in 0..3u32 {
                    let bitpos = 9 * rr + c;
                    if live & (1 << bitpos) != 0 {
                        assign(r, l, rm_cell(b, bitpos), d);
                        assigned = true;
                        break 'find;
                    }
                }
            }
        }
    }
    assigned
}

/// The packed baseline logic solver: 8 SIMD lanes, each running one query's cheap
/// closure in the SoA rep, refilled as it finishes via [`Self::run_stream`] (on demand
/// from the host) or [`Self::solve`] (from a slice). Holds resident warp state so the
/// warp can be **stepped** one pass at a time ([`Self::step_default`]) — scaffolding for a
/// two-warp host that drives it alongside a probe warp (the interleaved prototype that used
/// it was retired in favour of the unified warp); the type mirrors
/// [`PackedProber`](crate::probe::simt::PackedProber) so such a host owns one of each.
pub struct PackedSolver {
    r: [[V; 3]; 9],
    unsolved: [V; 3],
    active: [bool; LANES],
    counts: [[u16; NUM]; LANES],
}

impl Default for PackedSolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PackedSolver {
    pub fn new() -> Self {
        PackedSolver {
            r: [[ZERO; 3]; 9],
            unsolved: [ZERO; 3],
            active: [false; LANES],
            counts: [[0u16; NUM]; LANES],
        }
    }

    /// Whether any resident lane is still solving.
    #[inline]
    pub fn any_active(&self) -> bool {
        self.active.iter().any(|&a| a)
    }

    /// Whether baseline slot `l` is currently occupied (a stepping host fills idle ones).
    #[inline]
    pub fn slot_active(&self, l: usize) -> bool {
        self.active[l]
    }

    /// Load query `q` into slot `l` (initial fill or refill), marking it active and
    /// resetting its kind counts.
    #[inline]
    pub fn load(&mut self, l: usize, q: &SolveQuery) {
        load_query(&mut self.r, &mut self.unsolved, l, q);
        self.counts[l] = [0; NUM];
        self.active[l] = true;
    }

    /// ONE [`warp_pass_full`] over the resident active lanes + service. A lane that reaches
    /// a terminal verdict is **deactivated** and reported via `on_verdict(slot, trace)`
    /// (the caller refills via [`load`](Self::load) or leaves the slot idle); a stuck lane
    /// takes one scalar [`subset_step`] in place (rejoining the warp if a subset fired).
    /// The stepping primitive a two-warp host drives; the streaming [`drive`] is this
    /// in a loop with immediate refill. `VEC_LC`/`try_lc` mirror [`drive`]'s LC placement.
    fn step<const VEC_LC: bool, F: FnMut(usize, SolveTrace)>(
        &mut self,
        allowed: KindMask,
        try_lc: bool,
        mut on_verdict: F,
    ) {
        let active_mask = M::from_array(self.active);
        if !active_mask.any() {
            return;
        }
        sstat_add(0, 1);
        sstat_add(1, active_mask.to_bitmask().count_ones() as u64);
        let (changed, dead, solved) =
            warp_pass_full::<VEC_LC>(&mut self.r, &mut self.unsolved, active_mask);

        let active_b = active_mask.to_bitmask();
        let solved_b = solved.to_bitmask();
        let dead_b = dead.to_bitmask();
        let changed_b = changed.to_bitmask();
        let mut service = active_b & (solved_b | dead_b | !changed_b);
        while service != 0 {
            let l = service.trailing_zeros() as usize;
            service &= service - 1;
            let bit = 1u64 << l;
            let mut verdict: Option<bool> = None;
            if solved_b & bit != 0 {
                verdict = Some(true);
            } else if dead_b & bit != 0 {
                verdict = Some(false); // contradiction: unsolvable under the toolbox
            } else {
                match subset_step(&mut self.r, &mut self.unsolved, l, allowed, try_lc) {
                    Some(k) => self.counts[l][k] = self.counts[l][k].saturating_add(1),
                    None => verdict = Some(false), // no subset applies: unsolvable
                }
            }
            if let Some(v) = verdict {
                let trace = SolveTrace { solved: v, counts: self.counts[l] };
                self.active[l] = false;
                on_verdict(l, trace);
            }
        }
    }

    /// LC-off-warp stepping (the production default — see [`run_stream`](Self::run_stream))
    /// for a two-warp host: ONE pass + service, terminations reported via `on_verdict`.
    pub fn step_default<F: FnMut(usize, SolveTrace)>(&mut self, allowed: KindMask, on_verdict: F) {
        let try_lc = allowed & (1 << LC_POINTING) != 0;
        self.step::<false, F>(allowed, try_lc, on_verdict);
    }

    /// Drive the warp as a **streaming** baseline solver under the `allowed` toolbox:
    /// each freed SIMD lane is refilled by `next(slot, verdict)`. The callback receives
    /// the just-finished query's [`SolveTrace`] (`None` on the initial fill) and returns
    /// the next [`SolveQuery`] for that lane, or `None` when the lane has no more work.
    ///
    /// One iteration = one [`warp_pass_full`] over the active lanes + per-lane service:
    /// a solved lane is a verdict (`solved = true`); a dead lane is a verdict
    /// (`solved = false`, contradiction); a stalled lane (a full pass changed nothing,
    /// still unsolved) drops to a scalar [`subset_step`] — if a subset fires it rejoins
    /// the warp, else it is a verdict (`solved = false`, stuck); a still-changing lane
    /// keeps propagating in place. Subset counts accumulate per lane and ride out in the
    /// verdict's `counts`.
    pub fn run_stream<F>(&mut self, allowed: KindMask, next: F)
    where
        F: FnMut(usize, Option<SolveTrace>) -> Option<SolveQuery>,
    {
        // LC off the warp is the measured winner (train isolated 1.87x vs 1.36x for
        // LC-in-closure): the vectorized LC taxes every lane every pass, but ~70% of
        // calls never need LC, whereas the off-warp table LC only fires on the ~30% that
        // stall. See `examples/baselinebench` (the A/B knob).
        self.run_stream_with::<F>(allowed, false, next);
    }

    /// As [`run_stream`](Self::run_stream) but with `lc_in_closure` selecting where
    /// locked candidates run: `true` (default) vectorizes them in [`warp_pass_full`];
    /// `false` is the **LC-off-warp** experiment — the closure does only singles, and a
    /// stalled lane runs LC scalar before the subsets (so a lane needing LC drops out
    /// each time, like a subset step). Lets the bench A/B the two without a fork.
    pub fn run_stream_with<F>(&mut self, allowed: KindMask, lc_in_closure: bool, next: F)
    where
        F: FnMut(usize, Option<SolveTrace>) -> Option<SolveQuery>,
    {
        // Locked candidates are monomorphized away when the toolbox excludes them (drill),
        // and the fast path's both-or-neither precondition (mirroring `FusedLogicSolver`)
        // lets a single bit decide. Running LC for a no-LC baseline would over-solve, so
        // this gate is load-bearing, not just a speed knob.
        let lc_tool = allowed & (1 << LC_POINTING) != 0;
        debug_assert_eq!(
            lc_tool,
            allowed & (1 << LC_CLAIMING) != 0,
            "SIMT baseline fast path requires LC both-or-neither (mask {allowed:#b})"
        );
        if lc_tool && lc_in_closure {
            self.drive::<true, F>(allowed, next);
        } else {
            self.drive::<false, F>(allowed, next);
        }
    }

    fn drive<const VEC_LC: bool, F>(&mut self, allowed: KindMask, mut next: F)
    where
        F: FnMut(usize, Option<SolveTrace>) -> Option<SolveQuery>,
    {
        // When the closure does NOT vectorize LC but the toolbox has it, the scalar
        // stall step runs LC (before subsets) so a stalled lane still reaches the LC
        // fixpoint. With LC in the closure (or absent), the stall step skips it.
        let try_lc = !VEC_LC && (allowed & (1 << LC_POINTING) != 0);
        self.active = [false; LANES];

        // Initial fill: ask for one query per lane.
        for l in 0..LANES {
            if let Some(q) = next(l, None) {
                self.load(l, &q);
            }
        }
        // One pass + service per iteration, refilling each terminated slot on demand.
        // Terminations are collected from `step` (which can't re-borrow `self` to call
        // `next`) then refilled here — the lanes are independent, so deferring the refill
        // to after the pass is byte-identical to the inline refill it replaced.
        while self.any_active() {
            let mut ts = [0usize; LANES];
            let mut tt = [SolveTrace::default(); LANES];
            let mut tn = 0usize;
            self.step::<VEC_LC, _>(allowed, try_lc, |l, tr| {
                ts[tn] = l;
                tt[tn] = tr;
                tn += 1;
            });
            for i in 0..tn {
                let l = ts[i];
                if let Some(q) = next(l, Some(tt[i])) {
                    self.load(l, &q);
                }
            }
        }
    }

    /// Resolve a fixed batch of queries, writing `out[i]` = the [`SolveTrace`] for
    /// `queries[i]`. A thin [`Self::run_stream`] wrapper feeding `queries` in order —
    /// the isolated bench and the parity test. `out.len()` must be `>= queries.len()`.
    pub fn solve(&mut self, allowed: KindMask, queries: &[SolveQuery], out: &mut [SolveTrace]) {
        self.solve_with(allowed, false, queries, out);
    }

    /// As [`solve`](Self::solve) but selecting `lc_in_closure` (see
    /// [`run_stream_with`](Self::run_stream_with)) — the bench's A/B knob.
    pub fn solve_with(
        &mut self,
        allowed: KindMask,
        lc_in_closure: bool,
        queries: &[SolveQuery],
        out: &mut [SolveTrace],
    ) {
        let mut idx = 0usize;
        let mut lane_q = [0usize; LANES];
        self.run_stream_with(allowed, lc_in_closure, |slot, verdict| {
            if let Some(t) = verdict {
                out[lane_q[slot]] = t;
            }
            if idx < queries.len() {
                lane_q[slot] = idx;
                idx += 1;
                Some(queries[idx - 1])
            } else {
                None
            }
        });
    }
}

// ===========================================================================
// Unified warp: probe + baseline lanes in ONE warp
// ===========================================================================
//
// The two-warp hosts ([`crate::generate::random_simt`]) keep a [`PackedProber`] and a
// [`PackedSolver`] and try to feed the latter from the former. That starves the baseline
// warp: unique-gate boards trickle in too slowly to keep an 8-wide consumer full unless
// the host oversubscribes to L=64 macro-lanes (`simtutil`: baseline util 48%@16 ->
// 86%@64; probe ~100% throughout).
//
// `UnifiedWarp` removes the second warp entirely. Both gates run on the SAME 8 SIMD
// lanes, each lane tagged probe- or baseline-mode. The kernel is [`warp_pass_full`] (the
// baseline closure: naked + hidden singles incl. columns, LC off-warp), which is **sound
// for a probe lane too** — extra propagation only prunes the existence search, never
// changes the "does a completion exist" verdict. So a slot stays bound to its macro-lane
// and flips probe -> baseline **in place** the instant the prober reaches a unique
// verdict, with no batch, no inter-warp queue, and no oversubscription: the warp is full
// whenever work of EITHER kind exists, and probes are always plentiful. Active set = 8.
//
// Per-lane service diverges by mode (it already does in both engines — service is scalar):
// a probe lane branches / backtracks / yields a uniqueness verdict; a baseline lane drops
// to the scalar subset+LC step / yields a [`SolveTrace`]. The vectorized pass is uniform.

/// A resolved unified-warp lane's verdict, tagged by the mode the lane was in.
#[derive(Clone, Copy)]
pub enum UnifiedVerdict {
    /// A probe lane finished: `true` = an alternate completion exists (strip non-unique).
    Probe(bool),
    /// A baseline lane finished: the logic solver's trace (`solved` + subset-kind counts).
    Baseline(SolveTrace),
}

/// What the host hands a freed unified slot for its next pass.
pub enum UnifiedRefill {
    /// Load a uniqueness probe (the slot enters/continues probe mode).
    Probe(Probe),
    /// Load a baseline query (the slot enters baseline mode — typically the in-place
    /// probe->baseline flip when the just-finished probe was unique).
    Baseline(SolveQuery),
    /// No more work for this slot.
    Idle,
}

/// The unified probe+baseline warp (see the section comment). Holds the resident SoA
/// board plus per-lane mode, the prober's per-lane branch stacks, and the baseline's
/// per-lane subset-kind counts. Drives both gate kinds across the same 8 lanes.
pub struct UnifiedWarp {
    r: [[V; 3]; 9],
    unsolved: [V; 3],
    active: [bool; LANES],
    /// `true` = baseline mode, `false` = probe mode (per lane).
    baseline_mode: [bool; LANES],
    /// Probe-mode branch stacks (unused while a lane is in baseline mode).
    stacks: [Vec<Frame>; LANES],
    /// Baseline-mode subset-kind counts (reset on each baseline (re)load).
    counts: [[u16; NUM]; LANES],
    /// Kernel selector (the experiment #2 A/B): `false` = full closure
    /// ([`warp_pass_full`], columns vectorized for every lane); `true` = **lean** — the
    /// cheap [`warp_pass`] (naked + row/box singles, NO columns) for every lane, and columns
    /// recovered SCALAR per stalled baseline lane in the service loop ([`scalar_col_assign`],
    /// assign + let the smear place it). The bet: probe lanes (the majority of passes) never
    /// touch column ALU — SIMD or scalar — and only baseline lanes pay, on stall.
    ///
    /// (An earlier lean attempt recovered columns with a *masked SIMD* `warp_pass_full` pass
    /// — measured DEAD because the column fold runs warp-wide regardless of the mask, so it
    /// paid columns for everyone anyway, doing 32% more passes. The scalar recovery here is
    /// the genuine test of the lean premise: it never runs a full pass.)
    lean: bool,
}

impl Default for UnifiedWarp {
    fn default() -> Self {
        Self::new()
    }
}

impl UnifiedWarp {
    pub fn new() -> Self {
        Self::with_lean(false)
    }

    /// The **lean**-kernel variant (experiment #2): see [`UnifiedWarp::lean`].
    pub fn new_lean() -> Self {
        Self::with_lean(true)
    }

    fn with_lean(lean: bool) -> Self {
        UnifiedWarp {
            r: [[ZERO; 3]; 9],
            unsolved: [ZERO; 3],
            active: [false; LANES],
            baseline_mode: [false; LANES],
            stacks: core::array::from_fn(|_| Vec::with_capacity(64)),
            counts: [[0u16; NUM]; LANES],
            lean,
        }
    }

    #[inline]
    fn any_active(&self) -> bool {
        self.active.iter().any(|&a| a)
    }

    /// Load a probe into slot `l` (probe mode): the alts-restricted board + a cleared stack.
    #[inline]
    fn load_probe(&mut self, l: usize, p: &Probe) {
        load_lane(&mut self.r, &mut self.unsolved, l, p);
        self.stacks[l].clear();
        self.baseline_mode[l] = false;
        self.active[l] = true;
    }

    /// Load a baseline query into slot `l` (baseline mode): the plain stripped board + zeroed counts.
    #[inline]
    fn load_baseline(&mut self, l: usize, q: &SolveQuery) {
        load_query(&mut self.r, &mut self.unsolved, l, q);
        self.counts[l] = [0; NUM];
        self.baseline_mode[l] = true;
        self.active[l] = true;
    }

    /// ONE [`warp_pass_full`] over the active lanes + per-lane service dispatched by mode.
    /// Terminations are reported via `on_verdict(slot, UnifiedVerdict)`; a non-terminal
    /// probe lane branches/backtracks in place, a non-terminal baseline lane takes one
    /// scalar subset step in place.
    fn step<F: FnMut(usize, UnifiedVerdict)>(&mut self, allowed: KindMask, try_lc: bool, mut on_verdict: F) {
        let active_mask = M::from_array(self.active);
        if !active_mask.any() {
            return;
        }
        let active_b = active_mask.to_bitmask();
        uwstat_add(0, 1);
        uwstat_add(1, active_b.count_ones() as u64);
        // LC stays off the warp (the measured winner); a stalled baseline lane runs the
        // scalar LC fixpoint inside `subset_step` (gated by `try_lc`), and probe lanes
        // never need LC at all. So the closure is `warp_pass_full::<false>` (full) or, in
        // the lean experiment, the cheap `warp_pass` (no column hidden singles) — columns
        // are then recovered SCALAR, per stalled baseline lane, in the service loop below.
        let (changed, dead, solved) = if self.lean {
            warp_pass(&mut self.r, &mut self.unsolved, active_mask)
        } else {
            warp_pass_full::<false>(&mut self.r, &mut self.unsolved, active_mask)
        };

        let solved_b = solved.to_bitmask();
        let dead_b = dead.to_bitmask();
        let changed_b = changed.to_bitmask();
        let mut service = active_b & (solved_b | dead_b | !changed_b);
        while service != 0 {
            let l = service.trailing_zeros() as usize;
            service &= service - 1;
            let bit = 1u64 << l;
            let mut verdict: Option<UnifiedVerdict> = None;
            if self.baseline_mode[l] {
                if solved_b & bit != 0 {
                    verdict = Some(UnifiedVerdict::Baseline(SolveTrace { solved: true, counts: self.counts[l] }));
                } else if dead_b & bit != 0 {
                    verdict = Some(UnifiedVerdict::Baseline(SolveTrace { solved: false, counts: self.counts[l] }));
                } else if self.lean && scalar_col_assign(&mut self.r, &self.unsolved, l) {
                    // Lean kernel omits column hidden singles: recover them scalar for this
                    // one stalled baseline lane (assign each, the next `warp_pass` smears it
                    // in as a naked single). Made progress -> stay active, rejoin the warp,
                    // no verdict. Only fall to subsets once columns ALSO stall (full closure
                    // exhausted), matching `warp_pass_full`'s "stuck" point exactly.
                } else {
                    match subset_step(&mut self.r, &mut self.unsolved, l, allowed, try_lc) {
                        Some(k) => self.counts[l][k] = self.counts[l][k].saturating_add(1),
                        None => verdict = Some(UnifiedVerdict::Baseline(SolveTrace { solved: false, counts: self.counts[l] })),
                    }
                }
            } else if solved_b & bit != 0 {
                verdict = Some(UnifiedVerdict::Probe(true)); // a completion exists
            } else if dead_b & bit != 0 {
                if !backtrack_lane(&mut self.r, &mut self.unsolved, &mut self.stacks[l], l) {
                    verdict = Some(UnifiedVerdict::Probe(false)); // tree exhausted: unique
                }
            } else {
                branch_lane(&mut self.r, &mut self.unsolved, &mut self.stacks[l], l);
            }
            if let Some(v) = verdict {
                self.active[l] = false;
                on_verdict(l, v);
            }
        }
    }

    /// Drive the unified warp streaming: each freed lane is refilled by `next(slot,
    /// verdict)` (`None` verdict on the initial fill). The callback decides the slot's
    /// next load — a fresh/continuing probe, the baseline query for an in-place
    /// probe->baseline flip, or idle. One iteration = one [`step`](Self::step).
    pub fn run_stream<F>(&mut self, allowed: KindMask, mut next: F)
    where
        F: FnMut(usize, Option<UnifiedVerdict>) -> UnifiedRefill,
    {
        let try_lc = allowed & (1 << LC_POINTING) != 0;
        self.active = [false; LANES];
        for s in &mut self.stacks {
            s.clear();
        }
        for l in 0..LANES {
            match next(l, None) {
                UnifiedRefill::Probe(p) => self.load_probe(l, &p),
                UnifiedRefill::Baseline(q) => self.load_baseline(l, &q),
                UnifiedRefill::Idle => {}
            }
        }
        while self.any_active() {
            let mut ts = [0usize; LANES];
            let mut tv = [UnifiedVerdict::Probe(false); LANES];
            let mut tn = 0usize;
            self.step(allowed, try_lc, |l, v| {
                ts[tn] = l;
                tv[tn] = v;
                tn += 1;
            });
            for i in 0..tn {
                let l = ts[i];
                match next(l, Some(tv[i])) {
                    UnifiedRefill::Probe(p) => self.load_probe(l, &p),
                    UnifiedRefill::Baseline(q) => self.load_baseline(l, &q),
                    UnifiedRefill::Idle => {}
                }
            }
        }
    }
}
