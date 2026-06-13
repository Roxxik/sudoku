//! Packed SoA **baseline logic solver** machinery on the `repr` layer: the `solve`
//! analogue of [`probe::simt`](crate::probe::simt). Where the packed prober batches the
//! strip loop's *uniqueness* gates across W=8 SIMD lanes, this vectorizes its *baseline*
//! gates — the technique-driven, non-backtracking
//! [`FusedLogicSolver`](super::FusedLogicSolver) the strip runs per lane. The consumer is
//! the SIMT warp host ([`crate::generate::warp_host`]), which runs the probe and baseline
//! gates on one shared warp; this module provides the baseline closure ([`warp_pass_full`])
//! and the scalar harder-ladder fallback ([`ladder_step`]) it drives.
//!
//! ## The cheap-closure / subset split (data-driven)
//!
//! Measured on the production `train`/`drill(HiddenQuad)` specs (`examples/baselinestat`),
//! the baseline gate's work splits sharply: the **cheap closure** (naked + hidden
//! singles, locked candidates) SOLVES ~97% of calls in ~1 fixpoint pass, and only ~2%
//! ever reach the combinatorial **subset ladder** (naked/hidden pair..quad). So this
//! solver **vectorizes the cheap closure** ([`warp_pass_full`], lane-uniform band ALU)
//! and keeps the rare harder step **scalar per-lane** ([`ladder_step`], reusing the
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
//! LC runs **off the warp**: the closure does only singles, and a stalled lane runs the
//! fast table-driven scalar LC fixpoint ([`scalar_lc_fast`], the same `DROP_TRIP` LC
//! `FusedLogicSolver` uses) before the subsets, then rejoins. This beat a vectorized
//! in-closure LC round on the bench (train isolated **1.87x** vs **1.36x**): the closure's
//! LC would tax every lane every pass, but ~70% of calls never need LC (singles solve
//! them), whereas off-warp only pays LC on the ~30% that stall. (The in-closure variant
//! rode behind a knob on the standalone packed solver; both were removed.)
//!
//! Verdicts (`solved` + the subset-kind counts the spec's requirement check reads) are
//! identical to [`FusedLogicSolver`](super::FusedLogicSolver), so the closure is a
//! drop-in for the warp's baseline gate. Cheap-kind counts are not tracked (the fused
//! contract leaves them undefined, and the fast path's requirement check never reads
//! one — a Forced cheap kind routes off the fused/SIMT path entirely).

use super::combinations::for_each_combination;
use super::techniques;
use crate::counters::counter_block;
use crate::probe::simt::{
    BOX_CELLS, Frame, LANES, M, Probe, ROW_MASK, V, ZERO, assign, backtrack_lane, branch_lane,
    load_lane, one_bit, restore_lane, rm_cell, smear_v, snapshot_lane,
};
use crate::repr::banded::{Bands, RowMajor};
use crate::repr::{CellIdx, Digit, DigitGrid, Mark, Marks, Occupancy, SolverState, UNITS};
use crate::solve::Eliminate;
use crate::spec::kinds::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_TRIPLE, KindMask, LC_POINTING, NAKED_PAIR,
    NAKED_QUAD, NAKED_TRIPLE, SolveTrace,
};
use std::simd::cmp::SimdPartialEq;
use std::simd::num::SimdUint;
use std::simd::{Select, Simd};

// --- unified-warp utilization (feature = "count") -----------------------------
// The single warp the host runs probe AND baseline lanes on together
// ([`crate::generate::warp_host`]): [0] warp passes, [1] active-lane-sum over passes.
// util = [1]/(LANES*[0]). One number, not separate probe/baseline counters — the whole
// point of unifying is that the combined warp is full whenever either kind of work
// exists, so no oversubscription is needed to fill it. Read by `simtutil`.
counter_block!(UWSTAT: 2, inc = uwstat_inc, add = uwstat_add, snapshot = uwstat_snapshot, reset = uwstat_reset);

// --- harder-ladder memo effectiveness (feature = "count") ----------------------
// [0] ladder entries (ladder_step calls that reach the subset scans), [1] subset
// per-kind unit scans run, [2] subset per-kind unit scans skipped by the cross-stall
// memo, [3] fish per-kind digit scans run, [4] fish per-kind digit scans skipped by the
// per-digit memo, [5] cached fish-position digit slices rebuilt (the dirty share of the
// 81-cell scan the cache replaces). The skip shares [2]/([1]+[2]) (subsets) and
// [4]/([3]+[4]) (fishes) are the memo's bite per family.
counter_block!(LSTAT: 6, inc = lstat_inc, add = lstat_add, snapshot = lstat_snapshot, reset = lstat_reset);

/// Cross-stall memo for one lane's harder ladder (see [`ladder_step`]): the
/// unsolved-gated candidate bands as the ladder **last scanned them**, plus which
/// per-unit subset kinds and per-digit fish kinds have been verified no-fire on exactly
/// that state. A subset fires as a function of its unit's marks alone, and a basic fish
/// as a function of one digit's candidate positions alone, so a unit (resp. digit) whose
/// gated bands are unchanged since a kind scanned it clean cannot fire that kind — the
/// rescan is skipped and first-fire order is preserved exactly. Diffing the fresh
/// snapshot against `prev` on ladder entry catches every mutation source between stalls
/// uniformly (the previous fire's eliminations, the closure's placements and peer clears,
/// the off-warp LC prunes), so nothing on the closure spine needs to track changes.
///
/// The same per-digit granularity carries the fish step's *input*, not just its
/// verdicts: the per-digit candidate-position masks (`fish_pos`) are cached across
/// stalls and only the diff-dirtied digits' slices are rebuilt, so the unconditional
/// 81-cell `FishPositions::scan` drops out of the memoized path entirely.
pub(crate) struct LadderMemo {
    /// `candidates & unsolved` per digit band as last scanned (the state the
    /// `no_fire`/`fish_no_fire` verdicts are about).
    prev: [[u32; 3]; 9],
    /// Bit `k` = subset ladder kind `k` (pair..quad in difficulty order, see
    /// [`cellmarks_step_harder`]) verified no-fire on unit `u` for `prev`.
    no_fire: [u8; 27],
    /// Bit `k` = basic-fish kind `k` (X-Wing 0 / Swordfish 1 / Jellyfish 2) verified
    /// no-fire on digit `d` for `prev`. A fish is single-digit, so the memo is per
    /// digit, not per unit — see [`crate::solve::techniques::fish_step`].
    fish_no_fire: [u8; 9],
    /// The fish ladder's per-digit candidate-position masks, cached across stalls so
    /// the fish step's 81-cell board scan is not re-paid for digits the diff proved
    /// unchanged. Digits flagged in `fish_stale` are refreshed **lazily** from `prev`
    /// right before the fish scans run (so an entry that fires at LC or a subset pays
    /// nothing) — see [`cellmarks_step_harder`].
    fish_pos: techniques::FishPositions,
    /// Bit `d` = `fish_pos`'s digit-`d` slices do not reflect `prev` and must be
    /// rebuilt before the next fish scan reads them.
    fish_stale: u16,
    /// False after a lane (re)load: `prev`/`no_fire`/`fish_no_fire` describe a dead board.
    valid: bool,
}

impl LadderMemo {
    pub(crate) const INVALID: LadderMemo = LadderMemo {
        prev: [[0; 3]; 9],
        no_fire: [0; 27],
        fish_no_fire: [0; 9],
        fish_pos: techniques::FishPositions::EMPTY,
        fish_stale: 0x1FF,
        valid: false,
    };

    /// Mark the memo as describing a dead board (call on every lane (re)load).
    #[inline]
    pub(crate) fn invalidate(&mut self) {
        self.valid = false;
    }
}

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
pub(crate) fn load_query(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], l: usize, q: &SolveQuery) {
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
/// row/box hidden-single detection + [`smear_v`] placement reuse the prober's gather-free
/// geometry (the [`probe::simt`](crate::probe::simt) [`smear_v`]/[`one_bit`] kernel); the
/// **column** hidden singles are the one addition a non-backtracking solver needs (the
/// prober reaches columns by branching, so its pass omits them).
///
/// **Columns, gather-free.** A column `c` (0..9) holds its candidate cells at bit
/// positions `c`, `c+9`, `c+18` of each of the three bands. Folding those nine 9-bit
/// row-slices of digit `d`'s live board into per-column "seen once" / "seen twice"
/// gives the columns with exactly one live candidate — a hidden single. Broadcasting
/// that 9-bit column mask back across the three rows (`m | m<<9 | m<<18`) and AND-ing
/// the live board picks the single forced cell, in whichever band/row it sits, with no
/// transpose and no column-major view.
///
/// Locked candidates run **off the warp** (a stalled lane's scalar [`ladder_step`], via
/// [`scalar_lc_fast`]), so this closure does singles only.
#[cfg_attr(feature = "profiling", inline(never))]
pub(crate) fn warp_pass_full(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], active: M) -> (M, M, M) {
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
        // Balanced two-level tree instead of a 9-deep serial fold: collapse each
        // band's 3 row-slices into a saturating (ones>=1, twos>=2) pair, then merge
        // the 3 bands with the same identity (twos = ta|tb|(oa&ob); ones = oa|ob).
        // Critical-path depth drops ~9 -> ~4; col_single stays bit-identical.
        let mut bones = [ZERO; 3];
        let mut btwos = [ZERO; 3];
        for b in 0..3 {
            let live = r[d][b] & unsolved[b];
            let s0 = live & m9;
            let s1 = (live >> nine) & m9;
            let s2 = (live >> eighteen) & m9;
            let o01 = s0 | s1;
            bones[b] = o01 | s2;
            btwos[b] = (s0 & s1) | (o01 & s2);
        }
        let o01 = bones[0] | bones[1];
        let t01 = btwos[0] | btwos[1] | (bones[0] & bones[1]);
        let cones = o01 | bones[2];
        let ctwos = t01 | btwos[2] | (o01 & bones[2]);
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

    dead &= active;
    changed &= active;
    let empties = unsolved[0].count_ones() + unsolved[1].count_ones() + unsolved[2].count_ones();
    let solved = active & empties.simd_eq(ZERO) & !dead;
    (changed, dead, solved)
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
/// The nine 9-bit row slices of one digit's row-major bands (band `b` holds rows
/// `3b..3b+3` at bit offsets 0/9/18) — the [`FishPositions::rebuild_digit`]
/// (techniques::FishPositions::rebuild_digit) input, extracted here so the band
/// packing stays this module's knowledge.
#[inline]
fn band_rows(bands: &[u32; 3]) -> [u16; 9] {
    core::array::from_fn(|r| ((bands[r / 3] >> (9 * (r % 3))) & 0x1FF) as u16)
}

/// The six subsets share a single [`SubsetCache`] built up front (so the per-unit
/// transpose is paid once, not three times per kind); the rarer fish/wings — guarded
/// out entirely for the subset-only HiddenQuad toolbox — fall back to the generic
/// [`super::techniques`] bodies on the cell-major `cm` (every unit reachable via `get`,
/// columns included). `memo` is the lane's cross-stall [`LadderMemo`], already diffed
/// by [`ladder_step`]: the per-unit subset verdicts gate the subset scans, the
/// per-digit fish verdicts gate the fish scans, and the stale cached fish positions
/// are rebuilt here, lazily, from the diffed `prev` bands.
fn cellmarks_step_harder(
    cm: &mut CellMarks,
    allowed: KindMask,
    memo: &mut LadderMemo,
) -> Option<usize> {
    const ANY_SUBSET: KindMask = (1 << NAKED_PAIR)
        | (1 << HIDDEN_PAIR)
        | (1 << NAKED_TRIPLE)
        | (1 << HIDDEN_TRIPLE)
        | (1 << NAKED_QUAD)
        | (1 << HIDDEN_QUAD);
    // The cache only feeds the subset ladder; a subset-free toolbox (e.g. drill, whose
    // baseline is singles-only) skips the build entirely and falls straight through.
    // `no_fire` is the cross-stall memo (all-zero = scan everything); the ladder-order
    // bit per kind below indexes it.
    if allowed & ANY_SUBSET != 0 {
        lstat_inc(0);
        let cache = SubsetCache::build(cm);
        let no_fire = &mut memo.no_fire;
        macro_rules! try_kind {
            ($bit:expr, $call:expr) => {
                if allowed & (1 << $bit) != 0 && $call {
                    return Some($bit);
                }
            };
        }
        try_kind!(NAKED_PAIR, cached_naked_subset(cm, &cache, 2, 0, no_fire));
        try_kind!(HIDDEN_PAIR, cached_hidden_subset(cm, &cache, 2, 1, no_fire));
        try_kind!(NAKED_TRIPLE, cached_naked_subset(cm, &cache, 3, 2, no_fire));
        try_kind!(HIDDEN_TRIPLE, cached_hidden_subset(cm, &cache, 3, 3, no_fire));
        try_kind!(NAKED_QUAD, cached_naked_subset(cm, &cache, 4, 4, no_fire));
        try_kind!(HIDDEN_QUAD, cached_hidden_subset(cm, &cache, 4, 5, no_fire));
    }
    // Fish memo: refresh the cached positions for exactly the digits the entry diff
    // flagged stale, straight off the diffed bands (pure bit ops — no board re-read).
    // This sits AFTER the subsets on purpose: a subset fire returns above, so its entry
    // pays no fish maintenance and the staleness simply carries to a later entry; a
    // fishless toolbox (the `ANY_FISH` gate) never pays it at all. A stale digit always
    // has a cleared `no_fire` byte (same diff clears both), so no rebuild is wasted on
    // a digit the scans would skip.
    let fish_arg = if allowed & techniques::ANY_FISH != 0 {
        let mut w = memo.fish_stale;
        while w != 0 {
            let di = w.trailing_zeros() as usize;
            w &= w - 1;
            lstat_inc(5);
            memo.fish_pos.rebuild_digit(di, band_rows(&memo.prev[di]));
        }
        memo.fish_stale = 0;
        Some((&mut memo.fish_no_fire, &memo.fish_pos))
    } else {
        None
    };
    if let Some(k) = techniques::fish_step(cm, allowed, fish_arg) {
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
/// firing step, after which the ladder returns). `ladder_bit`/`no_fire` is the
/// cross-stall memo (see [`LadderMemo`]): units verified no-fire on an unchanged
/// board are skipped; a clean full scan of a unit sets its bit.
fn cached_naked_subset(
    cm: &mut CellMarks,
    cache: &SubsetCache,
    size: usize,
    ladder_bit: u8,
    no_fire: &mut [u8; 27],
) -> bool {
    let bit = 1u8 << ladder_bit;
    for u in 0..27 {
        if no_fire[u] & bit != 0 {
            lstat_inc(2);
            continue;
        }
        lstat_inc(1);
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
        no_fire[u] |= bit;
    }
    false
}

/// Cache-fed [`techniques::hidden_subset`] (see [`SubsetCache`]): identical first-fire
/// logic and elimination order, reading the precomputed per-unit position masks instead
/// of rebuilding the digit-position transpose per size. `ladder_bit`/`no_fire` as in
/// [`cached_naked_subset`].
fn cached_hidden_subset(
    cm: &mut CellMarks,
    cache: &SubsetCache,
    size: usize,
    ladder_bit: u8,
    no_fire: &mut [u8; 27],
) -> bool {
    let bit = 1u8 << ladder_bit;
    for u in 0..27 {
        if no_fire[u] & bit != 0 {
            lstat_inc(2);
            continue;
        }
        lstat_inc(1);
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
        no_fire[u] |= bit;
    }
    false
}

/// Snapshot stalled lane `l` out of the warp into a scalar [`SolverState`], run one
/// [`scalar_step_harder`] on it, and (if it fired) write the pruned board back into
/// the lane so the next [`warp_pass_full`] resumes the cheap closure. Returns the kind
/// that fired, or `None` when nothing applies (the lane is unsolvable under the
/// toolbox). Snapshot/restore is the prober's per-branch clone path; the harder search
/// is the rare ~2% tail, so paying it per-lane scalar is cheap.
///
/// `memo` is the lane's cross-stall [`LadderMemo`]: consecutive stalls of one baseline
/// solve change only a few cells, so the harder ladder skips every (kind, unit) subset
/// and (kind, digit) fish verified no-fire on a unit/digit whose gated bands are
/// unchanged since that scan — exact, first-fire preserved.
pub(crate) fn ladder_step(
    r: &mut [[V; 3]; 9],
    unsolved: &mut [V; 3],
    l: usize,
    allowed: KindMask,
    memo: &mut LadderMemo,
) -> Option<usize> {
    // The off-warp LC pass is in scope iff the toolbox allows it (pointing/claiming run
    // together); derived from `allowed` rather than carried as a redundant flag.
    let try_lc = allowed & (1 << LC_POINTING) != 0;
    let (mut sr, su) = snapshot_lane(r, unsolved, l);
    // LC-off-warp: run the FAST (table-driven) scalar LC fixpoint on the snapshot before
    // the subsets; if it pruned anything, rejoin the warp so the vectorized singles
    // resume. This is the fair off-warp comparison — same `DROP_TRIP` LC the scalar
    // `FusedLogicSolver` uses, not the slow composable one. The memo is untouched on
    // this early return: `prev` stays the last-SCANNED state, so the next entry's diff
    // sees the LC prunes (restored into the lane below) as dirt.
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
    // Cross-stall memo: diff the gated bands against the last-scanned state. Every
    // changed cell dirties its three units (clears their subset no-fire bits) and every
    // changed digit clears its fish no-fire byte AND marks its cached fish positions
    // stale — a fish is single-digit, so a digit whose positions are unchanged keeps
    // both its verdicts and its cached position masks. This catches the previous fire's
    // eliminations, the closure's work, and LC prunes uniformly. `prev` then becomes the
    // current state — the one the scans below run on (and the source the stale fish
    // slices are lazily rebuilt from, see `cellmarks_step_harder`).
    if memo.valid {
        let mut changed = [0u32; 3];
        for d in 0..9 {
            let mut dd = 0u32; // digit d's effective-band change, OR'd over bands
            for b in 0..3 {
                let eff = sr[d][b] & su[b];
                let x = eff ^ memo.prev[d][b];
                changed[b] |= x;
                dd |= x;
                memo.prev[d][b] = eff;
            }
            if dd != 0 {
                memo.fish_no_fire[d] = 0; // digit d moved -> all its fish verdicts stale
                memo.fish_stale |= 1 << d; // and its cached positions need a rebuild
            }
        }
        for (b, mut w) in changed.into_iter().enumerate() {
            while w != 0 {
                let cell = rm_cell(b, w.trailing_zeros());
                w &= w - 1;
                memo.no_fire[cell / 9] = 0;
                memo.no_fire[9 + cell % 9] = 0;
                memo.no_fire[18 + 3 * (cell / 27) + (cell % 9) / 3] = 0;
            }
        }
    } else {
        for d in 0..9 {
            for b in 0..3 {
                memo.prev[d][b] = sr[d][b] & su[b];
            }
        }
        memo.no_fire = [0; 27];
        memo.fish_no_fire = [0; 9];
        memo.fish_stale = 0x1FF;
        memo.valid = true;
    }
    let k = cellmarks_step_harder(&mut cm, allowed, memo)?;
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

/// Candidate-only **cell-major** board for the scalar harder step (see [`ladder_step`]):
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
    /// so [`ladder_step`] writes back only what the fired technique pruned (a targeted bit
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
        // SAFETY: `cell` is a `CellIdx` (0..81 by construction — UNITS/peer geometry
        // and the technique scans only ever index in range) and `marks` has 81 slots,
        // so the access is always in bounds. Eliding the check the compiler can't prove
        // (the index reaches `get` via const geometry tables it won't range-analyse).
        unsafe { *self.marks.get_unchecked(cell) }
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
#[allow(dead_code)]
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

// ===========================================================================
// Prober service + standalone prober driver
// ===========================================================================

/// One probe lane's scalar service after a [`warp_pass_full`]: the uniqueness verdict if
/// the lane terminated (`Some(true)` = an alternate completion exists -> non-unique;
/// `Some(false)` = the search tree is exhausted -> unique), or `None` if it keeps searching
/// (descended into a branch, or backtracked to an untried alternative). `solved`/`dead` are
/// this lane's bits from the pass. Shared by the host's
/// [`GateEngine`](crate::generate::warp_host::GateEngine) (which flips a unique lane to baseline
/// in place) and [`resolve_probes`] (which just reports the verdict), so the production
/// prober and the test/bench prober can't drift.
#[inline]
pub(crate) fn prober_service(
    r: &mut [[V; 3]; 9],
    unsolved: &mut [V; 3],
    stack: &mut Vec<Frame>,
    l: usize,
    solved: bool,
    dead: bool,
) -> Option<bool> {
    if solved {
        Some(true) // a completion exists: non-unique
    } else if dead {
        if backtrack_lane(r, unsolved, stack, l) {
            None // backtracked to an alternative: keep searching
        } else {
            Some(false) // tree exhausted: unique
        }
    } else {
        branch_lane(r, unsolved, stack, l);
        None // descended into a branch: keep searching
    }
}

/// Resolve a batch of uniqueness [`Probe`]s on the packed W=8 warp, returning per probe
/// whether an alternate completion exists (`true` = non-unique). This is the host's prober
/// phase in isolation: the same `warp_pass_full` pass and per-lane [`prober_service`], with
/// no baseline flip. The entry point the prober bench and the scalar/SIMT
/// verdict-equivalence test (`tests/prober_equiv`) drive — it pins exactly the verdict the
/// production warp computes, surfacing the "false unique" the host's auto-flip would
/// otherwise hide.
pub fn resolve_probes(probes: &[Probe]) -> Vec<bool> {
    let mut out = vec![false; probes.len()];
    if probes.is_empty() {
        return out;
    }
    let mut r = [[ZERO; 3]; 9];
    let mut unsolved = [ZERO; 3];
    let mut active = [false; LANES];
    let mut stacks: [Vec<Frame>; LANES] = core::array::from_fn(|_| Vec::with_capacity(64));
    let mut slot_probe = [0usize; LANES];
    let mut next = 0usize;

    // Initial fill: one probe per slot.
    for l in 0..LANES {
        if next >= probes.len() {
            break;
        }
        load_lane(&mut r, &mut unsolved, l, &probes[next]);
        stacks[l].clear();
        slot_probe[l] = next;
        active[l] = true;
        next += 1;
    }

    let mut active_mask = M::from_array(active);
    while active_mask.any() {
        let (changed, dead, solved) = warp_pass_full(&mut r, &mut unsolved, active_mask);
        let active_b = active_mask.to_bitmask();
        let solved_b = solved.to_bitmask();
        let dead_b = dead.to_bitmask();
        let changed_b = changed.to_bitmask();
        let mut service = active_b & (solved_b | dead_b | !changed_b);
        while service != 0 {
            let l = service.trailing_zeros() as usize;
            service &= service - 1;
            let bit = 1u64 << l;
            if let Some(nonunique) = prober_service(
                &mut r,
                &mut unsolved,
                &mut stacks[l],
                l,
                solved_b & bit != 0,
                dead_b & bit != 0,
            ) {
                out[slot_probe[l]] = nonunique;
                // Refill the freed slot with the next probe, or idle it.
                if next < probes.len() {
                    load_lane(&mut r, &mut unsolved, l, &probes[next]);
                    stacks[l].clear();
                    slot_probe[l] = next;
                    next += 1;
                } else {
                    active[l] = false;
                }
            }
        }
        active_mask = M::from_array(active);
    }
    out
}

/// A freed warp lane's verdict — the resume value the host hands back to its lane
/// coroutine. The unique-probe case never appears here: on a unique verdict the warp
/// flips the lane to its baseline phase *in place* (see
/// [`GateEngine`](crate::generate::warp_host::GateEngine)), so the host only ever sees a
/// non-unique probe or a finished baseline.
#[derive(Clone, Copy)]
pub enum GateResult {
    /// A probe lane found an alternate completion — the strip is non-unique.
    ProbeNonUnique,
    /// A baseline lane finished: the logic solver's trace (`solved` + subset-kind counts).
    Baseline(SolveTrace),
}

// ===========================================================================
// branch_lane frame-fusion criterion microbench (lab-only)
// ===========================================================================
//
// `branch_lane` snapshots a stalled lane's board straight into its pushed [`Frame`]
// (the frame-fusion win — it used to copy the 31-word board a second time, local ->
// `Vec` slot). This harness is the standing criterion for that per-branch-node cost:
// it harvests realistic pre-branch boards from a probe corpus, then replays one branch
// op per iteration over them. The board load (`restore_lane`) and the read-back
// checksum are fixed overhead, so the headline number is the SDE `-mix` per-call
// `branch_lane` instruction count; the checksum is a DCE guard. To A/B a future change,
// keep the prior `branch_lane` body alongside and drive both (see git history for the
// original three-way harness that proved the fusion).

/// A captured pre-branch lane board (a [`snapshot_lane`] result): the exact stalled
/// state at which `branch_lane` fires.
pub type BranchInput = ([[u32; 3]; 9], [u32; 3]);

/// Harvest realistic `branch_lane` inputs from `probes`: load each probe into a lane,
/// run the cheap warp closure to its first stall, and snapshot the stalled board (the
/// exact state `branch_lane` would see). Probes that solve or die before stalling
/// contribute nothing.
pub fn collect_branch_inputs(probes: &[Probe]) -> Vec<BranchInput> {
    let mut out = Vec::new();
    let mut r = [[ZERO; 3]; 9];
    let mut unsolved = [ZERO; 3];
    let only0 = {
        let mut a = [false; LANES];
        a[0] = true;
        M::from_array(a)
    };
    for p in probes {
        load_lane(&mut r, &mut unsolved, 0, p);
        // Drive lane 0 alone to its first stall (bounded against pathological inputs).
        for _ in 0..256 {
            let (changed, dead, solved) = warp_pass_full(&mut r, &mut unsolved, only0);
            let bit = 1u64;
            if (solved.to_bitmask() & bit) != 0 || (dead.to_bitmask() & bit) != 0 {
                break; // terminated before branching — no input here
            }
            if (changed.to_bitmask() & bit) == 0 {
                out.push(snapshot_lane(&r, &unsolved, 0)); // stalled: a real branch point
                break;
            }
        }
    }
    out
}

/// Replay `iters` [`branch_lane`] ops over `inputs` (cycled). Each iteration loads an
/// input into lane 0 via [`restore_lane`], branches, then folds the pushed frame and the
/// post-assign lane into a checksum. Returns the checksum — stable run-to-run (a DCE
/// guard, not a result to interpret). The load + fold are fixed overhead; the headline
/// number is the SDE `-mix` per-call `branch_lane` instruction count.
pub fn run_branch_bench(inputs: &[BranchInput], iters: usize) -> u64 {
    assert!(!inputs.is_empty(), "run_branch_bench needs a non-empty corpus");
    let mut r = [[ZERO; 3]; 9];
    let mut unsolved = [ZERO; 3];
    let mut stack: Vec<Frame> = Vec::with_capacity(64);
    let n = inputs.len();
    let mut acc: u64 = 0;
    for i in 0..iters {
        let (sr, su) = &inputs[i % n];
        restore_lane(&mut r, &mut unsolved, 0, sr, su);
        stack.clear();
        branch_lane(&mut r, &mut unsolved, &mut stack, 0);
        // Read back the pushed frame + the post-assign lane 0 into the checksum word.
        let mut h = stack.last().unwrap().fold_hash();
        for d in 0..9 {
            for b in 0..3 {
                h ^= (r[d][b].as_array()[0] as u64).rotate_left((d + b + 1) as u32);
            }
        }
        for b in 0..3 {
            h ^= (unsolved[b].as_array()[0] as u64).rotate_left((b + 5) as u32);
        }
        acc ^= h.rotate_left((i & 63) as u32);
    }
    acc
}
