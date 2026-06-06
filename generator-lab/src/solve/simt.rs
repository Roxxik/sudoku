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

use super::techniques;
use crate::probe::simt::{
    BOX_CELLS, LANES, M, ONE, ROW_MASK, V, ZERO, one_bit, restore_lane, smear_v, snapshot_lane,
};
use crate::repr::banded::{Bands, RowMajor};
use crate::repr::{DigitGrid, Marks, PerDigit, SolverState};
use crate::solve::LogicBoard;
use crate::spec::kinds::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_TRIPLE, KindMask, LC_CLAIMING, LC_POINTING, NAKED_PAIR,
    NAKED_QUAD, NAKED_TRIPLE, NUM, SolveTrace,
};
use std::simd::cmp::SimdPartialEq;
use std::simd::num::SimdUint;
use std::simd::{Select, Simd};

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

/// The discrete subset ladder, easiest-first, gated by `allowed` — the scalar
/// per-lane fallback the warp drops a stalled lane to. Mirror of
/// [`FusedLogicSolver`](super::FusedLogicSolver)'s `step_subsets`: the cheap closure
/// has already drained singles + LC, so only NakedPair..HiddenQuad remain, and it runs
/// on a single row-major [`SolverState`] (every unit reachable via `get`, columns
/// included). Returns the kind index of the first subset that fired, or `None`.
fn scalar_step_subsets<B: LogicBoard>(b: &mut B, allowed: KindMask) -> Option<usize> {
    macro_rules! try_subset {
        ($bit:expr, $call:expr) => {
            if allowed & (1 << $bit) != 0 && $call {
                return Some($bit);
            }
        };
    }
    try_subset!(NAKED_PAIR, techniques::naked_subset(b, 2));
    try_subset!(HIDDEN_PAIR, techniques::hidden_subset(b, 2));
    try_subset!(NAKED_TRIPLE, techniques::naked_subset(b, 3));
    try_subset!(HIDDEN_TRIPLE, techniques::hidden_subset(b, 3));
    try_subset!(NAKED_QUAD, techniques::naked_subset(b, 4));
    try_subset!(HIDDEN_QUAD, techniques::hidden_subset(b, 4));
    None
}

/// Snapshot stalled lane `l` out of the warp into a scalar [`SolverState`], run one
/// [`scalar_step_subsets`] on it, and (if it fired) write the pruned board back into
/// the lane so the next [`warp_pass_full`] resumes the cheap closure. Returns the kind
/// that fired, or `None` when no subset applies (the lane is unsolvable under the
/// toolbox). Snapshot/restore is the prober's per-branch clone path; the subset search
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
    let cand = PerDigit::new(core::array::from_fn(|d| {
        Bands::<RowMajor>::from_lanes([sr[d][0], sr[d][1], sr[d][2], 0])
    }));
    let mut ss = SolverState::from_parts(cand, Bands::<RowMajor>::from_lanes([su[0], su[1], su[2], 0]));
    let k = scalar_step_subsets(&mut ss, allowed)?;
    let nr: [[u32; 3]; 9] = core::array::from_fn(|d| {
        let t = ss.candidates().each()[d].to_lanes();
        [t[0], t[1], t[2]]
    });
    let t = ss.unsolved().to_lanes();
    restore_lane(r, unsolved, l, &nr, &[t[0], t[1], t[2]]);
    Some(k)
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

/// The packed baseline logic solver: 8 SIMD lanes, each running one query's cheap
/// closure in the SoA rep, refilled as it finishes via [`Self::run_stream`] (on demand
/// from the host) or [`Self::solve`] (from a slice). Stateless today (the subset
/// fallback snapshots on demand); the type mirrors [`PackedProber`](crate::probe::simt::PackedProber)
/// so the warp host can own one of each.
pub struct PackedSolver;

impl Default for PackedSolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PackedSolver {
    pub fn new() -> Self {
        PackedSolver
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
        let mut r = [[ZERO; 3]; 9];
        let mut unsolved = [ZERO; 3];
        let mut active = [false; LANES];
        let mut counts = [[0u16; NUM]; LANES];

        // Initial fill: ask for one query per lane.
        for l in 0..LANES {
            if let Some(q) = next(l, None) {
                load_query(&mut r, &mut unsolved, l, &q);
                counts[l] = [0; NUM];
                active[l] = true;
            }
        }

        let mut active_mask = M::from_array(active);
        while active_mask.any() {
            let (changed, dead, solved) = warp_pass_full::<VEC_LC>(&mut r, &mut unsolved, active_mask);

            // Service only the lanes that reached a decision: solved, dead, or stuck
            // (active but unchanged). A lane still propagating is skipped (the common
            // case). Verdict masks reduced to integers once, as in the prober.
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
                    // Stuck at the cheap fixpoint with cells left: try one scalar step.
                    match subset_step(&mut r, &mut unsolved, l, allowed, try_lc) {
                        Some(k) => counts[l][k] = counts[l][k].saturating_add(1),
                        None => verdict = Some(false), // no subset applies: unsolvable
                    }
                }

                if let Some(v) = verdict {
                    let trace = SolveTrace { solved: v, counts: counts[l] };
                    if let Some(q) = next(l, Some(trace)) {
                        load_query(&mut r, &mut unsolved, l, &q);
                        counts[l] = [0; NUM];
                        // active[l] stays true
                    } else {
                        active[l] = false;
                    }
                }
            }
            active_mask = M::from_array(active);
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
