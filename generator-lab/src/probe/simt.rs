//! Packed SoA prober on the `repr` layer: the [`crate::generate::random_simt`] batch
//! point. Instead of K scalar uniqueness queries, it lays them out across SIMD lanes
//! and propagates them in lockstep — W=8 independent existence DFS searches, one per
//! lane, fed by a work-stealing refill queue so a lane that reaches a verdict
//! immediately pulls the next probe (the perfect-refill regime, ~2.6x per core).
//!
//! Its [`Probe`] SoA is loaded from a
//! [`SolverState<Bands<RowMajor>>`](crate::repr::SolverState) row view (via
//! [`crate::repr::banded::Bands::to_lanes`]). The row-major banding lays cells out
//! as `cell = 27*band + pos` ([`RowMajor`]), so the gather-free smear+ALU kernel and
//! its bit twiddling read straight off the bands; the cell <-> (lane, bit) helpers
//! come from the [`Banding`] geometry.
//!
//! ## The gather-free kernel (the lever that makes it pay)
//!
//! The scalar prober is a backtracking existence DFS: propagate to a fixpoint,
//! and when that stalls, branch. The bulk of the cost is propagation, which is
//! *lane-uniform* — the same `Simd<u32, LANES>` band ops on every lane — so it
//! packs. The earlier packed prober packed the *detection* (the naked-single
//! sieve) but still did **placement** and **hidden singles** per lane with
//! `PEER_MASK_R[cell]` / `SINGLE9` table gathers — and those gathers were the
//! wall (a per-lane scatter on the hot path, ~6.5x the scalar per-node cost).
//!
//! This kernel removes every gather from the propagation primitive:
//!
//! - **Naked + hidden singles in one vectorized pass** ([`warp_pass`]). Hidden
//!   singles are detected by ALU (`v != 0 & v & (v-1) == 0`, [`one_bit`]) over
//!   the in-lane row/box fields — **not** the `SINGLE9` table (a cross-lane
//!   gather). Columns straddle bands and are simply never swept as units;
//!   branching still reaches every completion, so the verdict is unchanged.
//! - **Placement via the smear formula** ([`smear_v`]). The peer union of a placed
//!   wave is computed as uniform band ops (row-smear | box-expand |
//!   column-occupancy broadcast `occ | occ<<9 | occ<<18`), with **no per-cell peer gather**.
//! - **Conflict via peer popcount** (inside [`smear_v`]) — a placed group whose
//!   cells collide in a unit (distinct touched units < group size) is dead, read
//!   straight off the peer masks (`popcount(row_peer) < 9*n`), gather-free.
//! - **Branch via [`assign`]** — restrict the branch cell to one digit and let the
//!   next sweep place it (smear). No per-branch peer gather.
//!
//! The only per-lane scalar work left is the residue the design predicts is
//! irreducible: the branch-cell pick ([`branch_cell`]) and the per-branch state
//! snapshot / restore ([`Frame`]). The closure — the dominant cost — is now fully
//! uniform across the warp.
//!
//! Verdicts are identical to the scalar [`Search`](crate::probe::Search) prober
//! (existence is order- and technique-independent), so the warp stays byte-identical
//! to the sequential [`crate::generate`] generator (the equivalence test pins it).

use crate::repr::banded::{Banding, RowMajor};
use std::simd::cmp::{SimdPartialEq, SimdPartialOrd};
use std::simd::num::SimdUint;
use std::simd::{Mask, Select, Simd};

// --- row-major cell <-> (lane, bit) maps, re-sourced from the `repr` banding ------
// The packed prober speaks in (lane, bit); the [`RowMajor`] [`Banding`] owns the
// geometry (`band_idx`/`band_pos`/`cell_at`), byte-identical to the old bb
// `rm_lane`/`rm_bit`/`rm_cell` it replaces. Thin wrappers keep the kernel body unchanged.
#[inline]
pub(crate) fn rm_lane(cell: usize) -> usize {
    RowMajor::band_idx(cell)
}
#[inline]
pub(crate) fn rm_bit(cell: usize) -> usize {
    RowMajor::band_pos(cell)
}
#[inline]
pub(crate) fn rm_cell(lane: usize, bit: u32) -> usize {
    RowMajor::cell_at(lane, bit as usize)
}

/// Lanes per warp. 8 = one AVX2 `Simd<u32, 8>` register per band-word. (16 / AVX-512
/// was a wash: Zen 4 double-pumps it, so no raw-throughput gain.)
pub const LANES: usize = 8;

pub(crate) type V = Simd<u32, LANES>;
pub(crate) type M = Mask<i32, LANES>;
pub(crate) const ZERO: V = Simd::from_array([0; LANES]);
pub(crate) const ONE: V = Simd::from_array([1; LANES]);

/// The three rows of a band as 9-bit masks (row r occupies bits `9*r..9*r+9`).
pub(crate) const ROW_MASK: [u32; 3] = [0x1FF, 0x1FF << 9, 0x1FF << 18];
/// The three boxes of a band: box k is columns `3*k..3*k+3` across all three rows.
pub(crate) const BOX_CELLS: [u32; 3] = {
    let mut t = [0u32; 3];
    let mut k = 0;
    while k < 3 {
        let c3 = 0b111u32 << (3 * k);
        t[k] = c3 | (c3 << 9) | (c3 << 18);
        k += 1;
    }
    t
};

/// Lane-parallel "exactly one bit set" over a per-lane field: `f != 0 && f &
/// (f-1) == 0`. The lanes whose field holds a lone bit (a hidden single).
#[inline(always)]
pub(crate) fn one_bit(x: V) -> M {
    x.simd_ne(ZERO) & (x & (x - ONE)).simd_eq(ZERO)
}

/// The peer union of a placed group (the cells to clear digit `d` from) plus the
/// per-lane contradiction mask, as uniform band ALU — no per-cell `PEER_MASK` gather.
///
/// For each placed cell: its whole row is a peer (row-smear), its whole box is a
/// peer (box-expand), and its column across all three bands is a peer (fold each
/// band to a 9-bit column-occupancy, OR across bands, broadcast back to all rows
/// via `occ | occ<<9 | occ<<18`). The column term is shared by all three bands;
/// the row/box terms are per band.
///
/// A group is contradictory when its cells collide in a unit (two singles for the
/// same digit in one unit) — distinct touched units < cell count. That is read
/// straight off the peer masks instead of separate occupancy fields: distinct rows
/// partition into disjoint 9-bit `ROW_MASK`s, so `popcount(rp) == 9 * rows_touched`
/// and the row collision is `popcount(rp) < 9*n`; likewise boxes; columns use the
/// 9-bit `col_occ` directly (`popcount(col_occ) < n`). This drops the 18 per-pass
/// occupancy selects the old `row_occ`/`box_occ` accumulation paid.
#[inline]
pub(crate) fn smear_v(group: [V; 3]) -> ([V; 3], M) {
    let m9 = Simd::splat(0x1FF);
    let mut col_occ = ZERO;
    for b in 0..3 {
        col_occ |= (group[b] & m9)
            | ((group[b] >> Simd::splat(9)) & m9)
            | ((group[b] >> Simd::splat(18)) & m9);
    }
    let colpeer = col_occ | (col_occ << Simd::splat(9)) | (col_occ << Simd::splat(18));
    let mut out = [ZERO; 3];
    let mut rp_pop = ZERO;
    let mut bp_pop = ZERO;
    for b in 0..3 {
        let g = group[b];
        let mut rp = ZERO;
        for i in 0..3 {
            let rm = Simd::splat(ROW_MASK[i]);
            rp |= (g & rm).simd_ne(ZERO).select(rm, ZERO);
        }
        let mut bp = ZERO;
        for k in 0..3 {
            let bm = Simd::splat(BOX_CELLS[k]);
            bp |= (g & bm).simd_ne(ZERO).select(bm, ZERO);
        }
        rp_pop += rp.count_ones();
        bp_pop += bp.count_ones();
        out[b] = rp | bp | colpeer;
    }
    // distinct rows/boxes touched = popcount(peer)/9; collide iff < n  <=>  popcount < 9n.
    let n = group[0].count_ones() + group[1].count_ones() + group[2].count_ones();
    let n9 = n * Simd::splat(9);
    let conflict = rp_pop.simd_lt(n9) | bp_pop.simd_lt(n9) | col_occ.count_ones().simd_lt(n);
    (out, conflict)
}

use crate::counters::counter_block;

// --- packed-prober verdict split (feature = "count") --------------------------
// How probes resolve under the new kernel: settled by propagation alone (no
// branch ever needed) vs needing the DFS — [solved-no-branch, contra-no-branch,
// branched]. `packdiag` reads this via `pstat_snapshot`; `pstat_bump(i)` tallies.
// The `branched` flag is only tracked under the feature so production pays nothing.
counter_block!(PSTAT: 3, inc = pstat_bump, add = pstat_add, snapshot = pstat_snapshot, reset = pstat_reset);

// --- packed-DFS prober metrics (feature = "count") ----------------------------
// [0] probes, [1] warp passes (ticks), [2] branch nodes, [3] active-lane-sum over
// ticks. utilization = active_lane_sum / (LANES * ticks); branches/probe and
// passes/probe size the node inflation vs the scalar prober (`infl`). `dstat_add(i, v)`
// tallies; it is a no-op when the feature is off, so its call sites need no gating.
counter_block!(DSTAT: 4, inc = dstat_inc, add = dstat_add, snapshot = dstat_snapshot, reset = dstat_reset);

// Per-lane "did the just-resolved probe ever branch" flag, stashed right before the
// refill callback so a count-instrumented host can correlate a gate's probe-branch with
// its baseline outcome (the "no-branch => baseline-trivial?" study). Single-threaded.
#[cfg(feature = "count")]
static mut LAST_BRANCHED: [bool; LANES] = [false; LANES];
/// Whether the probe slot `l` just resolved had to branch (read it in the refill
/// callback, immediately after receiving the verdict). `feature = "count"` only.
#[cfg(feature = "count")]
pub fn last_branched(l: usize) -> bool {
    // SAFETY: single-threaded lab.
    unsafe { LAST_BRANCHED[l] }
}

/// One lane's input to the packed prober: its row-major candidate bands and empty
/// mask (from a [`SolverState<Bands<RowMajor>>`](crate::repr::SolverState) row view,
/// via [`crate::repr::banded::Bands::to_lanes`]), the stripped `cell`, and the
/// alternate-digit mask the cell is restricted to.
#[derive(Clone, Copy)]
pub struct Probe {
    pub r: [[u32; 4]; 9],
    pub unsolved: [u32; 4],
    pub cell: usize,
    pub alts: u16,
}

impl Probe {
    /// An inert padding probe (no candidates, nothing unsolved) for filling a
    /// fixed `[Probe; LANES]` scratch array beyond the live count.
    pub const EMPTY: Probe = Probe { r: [[0; 4]; 9], unsolved: [0; 4], cell: 0, alts: 0 };
}

/// Load probe `p` into lane `l` of the SoA boards, restricting its `cell` to the
/// alternate digits `alts` (clear the non-alternate digits' bit there). This is
/// the refill path — the per-lane scalar store of a fresh query.
#[inline]
pub(crate) fn load_lane(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], l: usize, p: &Probe) {
    let cl = rm_lane(p.cell);
    let cbit = 1u32 << rm_bit(p.cell);
    // The cell's band gets a per-digit AND-mask that clears `cbit` for every non-alternate
    // digit; the other two bands are untouched. Done branchlessly: the per-digit `if keep`
    // is a data-dependent test on `alts` (9 unpredictable branches/probe) and was a refill
    // mispredict source. `keep.wrapping_neg()` is all-ones when the digit is an alternate
    // (so the mask is `!0`, no clear) and zero otherwise (mask `!cbit`); `bmask[cl]` is an
    // indexed store, so no `b == cl` branch either. The stores carry no dependency.
    for d in 0..9 {
        let keep = ((p.alts >> d) & 1) as u32;
        let cl_mask = !cbit | keep.wrapping_neg();
        let mut bmask = [u32::MAX; 3];
        bmask[cl] = cl_mask;
        for b in 0..3 {
            r[d][b].as_mut_array()[l] = p.r[d][b] & bmask[b];
        }
    }
    for b in 0..3 {
        unsolved[b].as_mut_array()[l] = p.unsolved[b];
    }
}

/// Snapshot lane `l`'s board out of the SoA warp into scalar bands (the per-branch
/// clone the design pays). Strided reads of 30 `u32`, no scatter.
#[inline]
pub(crate) fn snapshot_lane(r: &[[V; 3]; 9], unsolved: &[V; 3], l: usize) -> ([[u32; 3]; 9], [u32; 3]) {
    (
        core::array::from_fn(|d| core::array::from_fn(|b| r[d][b].as_array()[l])),
        core::array::from_fn(|b| unsolved[b].as_array()[l]),
    )
}

/// Restore a snapshot into lane `l` of the SoA warp (backtrack).
#[inline]
pub(crate) fn restore_lane(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], l: usize, sr: &[[u32; 3]; 9], su: &[u32; 3]) {
    for d in 0..9 {
        for b in 0..3 {
            r[d][b].as_mut_array()[l] = sr[d][b];
        }
    }
    for b in 0..3 {
        unsolved[b].as_mut_array()[l] = su[b];
    }
}

/// Assume digit `dd` (0-based) at lane `l`'s cell `(lane=cl, bit)`: clear every
/// other digit's candidate there. The cell stays unsolved — the next [`warp_pass`]
/// sees it as a naked single and places it via the smear (gather-free branch).
#[inline]
pub(crate) fn assign(r: &mut [[V; 3]; 9], l: usize, cell: usize, dd: usize) {
    let cl = rm_lane(cell);
    let cbit = 1u32 << rm_bit(cell);
    for d in 0..9 {
        // Branchless: clear `cbit` for every digit but `dd`. The `if d != dd` is a
        // data-dependent branch (dd varies per branch node); `keep.wrapping_neg()` is
        // all-ones at `d == dd` (mask `!0`, no-op) and zero elsewhere (mask `!cbit`).
        let keep = (d == dd) as u32;
        r[d][cl].as_mut_array()[l] &= !cbit | keep.wrapping_neg();
    }
}

/// ONE propagation pass — a single per-digit placement sweep that fuses naked and
/// hidden singles — across the warp, applied only to `active` lanes, fully vectorized
/// (smear placement, ALU hidden detection, popcount conflict). Returns per-lane
/// `(changed, dead, solved)`. The scheduler calls this repeatedly; a lane is at
/// fixpoint when a full pass leaves it unchanged. Same forced-cell set as the scalar
/// closure, so the per-lane fixpoint is identical (the closure is confluent) — only the
/// pass granularity differs.
#[cfg_attr(feature = "profiling", inline(never))]
pub(crate) fn warp_pass(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], active: M) -> (M, M, M) {
    let mut changed = M::splat(false);
    let mut dead = M::splat(false);

    // Naked singles, one sweep: ones = "at least one candidate", twos = "at least
    // two", per band, accumulated across the nine digit boards — lane-parallel.
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
        dead |= (unsolved[b] & !ones[b]).simd_ne(ZERO); // a cell with no candidate
        singles[b] = unsolved[b] & ones[b] & !twos[b];
    }
    // One placement sweep per digit, fusing naked and hidden singles into a single
    // `smear_v` (the dominant primitive) — halving the smear count from 18 to 9 vs a
    // separate naked-then-hidden pass. For each digit the forced group is its naked
    // singles (`singles & r[d]`) unioned with its row/box hidden singles, detected on
    // the *current* board. Each digit places immediately, so a later digit's hidden
    // detection reads the already-updated `unsolved` and can't re-place a solved cell —
    // every placed cell is genuinely forced, the closure stays confluent, and the
    // per-lane verdict is unchanged. The loop is left rolled deliberately: unrolling the
    // nine large (inlined-`smear_v`) bodies blew up code size and thrashed the I-cache
    // for a net loss, far outweighing the one loop-exit mispredict per pass it would save.
    for d in 0..9 {
        let mut group = [singles[0] & r[d][0], singles[1] & r[d][1], singles[2] & r[d][2]];
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

/// A saved search state for one branch point of one lane: the board (row-major
/// bands + empty mask) as it was *before* any branch digit was tried at `cell`,
/// and `remaining` = the candidate digits not yet tried there.
#[derive(Clone, Copy)]
pub(crate) struct Frame {
    r: [[u32; 3]; 9],
    unsolved: [u32; 3],
    cell: u32,
    remaining: u16,
}

/// The packed-DFS uniqueness prober: 8 SIMD lanes, each running one probe's
/// existence search in the SoA rep, refilled as it finishes via
/// [`Self::run_stream`] (on demand from the host) or [`Self::resolve`] (from a
/// slice). Owns the reusable per-lane branch stacks so repeated calls don't churn
/// allocations.
pub struct PackedProber {
    stacks: [Vec<Frame>; LANES],
    // Resident warp state (was local to `run_stream`) so the warp can be **stepped** one
    // pass at a time — scaffolding for a two-warp host driving this alongside a baseline warp
    // (the interleaved prototype that used it was retired for the unified warp); `run_stream`
    // is just `step` in a loop with immediate refill.
    r: [[V; 3]; 9],
    unsolved: [V; 3],
    active: [bool; LANES],
    /// Per-lane "this probe ever branched" flag (the packdiag split / `last_branched`).
    branched: [bool; LANES],
}

impl Default for PackedProber {
    fn default() -> Self {
        Self::new()
    }
}

impl PackedProber {
    pub fn new() -> Self {
        PackedProber {
            stacks: core::array::from_fn(|_| Vec::with_capacity(64)),
            r: [[ZERO; 3]; 9],
            unsolved: [ZERO; 3],
            active: [false; LANES],
            branched: [false; LANES],
        }
    }

    /// Whether any resident lane is still searching.
    #[inline]
    pub fn any_active(&self) -> bool {
        self.active.iter().any(|&a| a)
    }

    /// Whether probe slot `l` is currently occupied (a stepping host fills idle slots).
    #[inline]
    pub fn slot_active(&self, l: usize) -> bool {
        self.active[l]
    }

    /// Load probe `p` into slot `l` (initial fill or refill), marking it active.
    #[inline]
    pub fn load(&mut self, l: usize, p: &Probe) {
        load_lane(&mut self.r, &mut self.unsolved, l, p);
        self.stacks[l].clear();
        self.branched[l] = false;
        self.active[l] = true;
        dstat_add(0, 1);
    }

    /// ONE [`warp_pass`] over the resident active lanes + service. A lane that reaches a
    /// terminal verdict this pass is **deactivated** and reported via `on_verdict(slot,
    /// nonunique)` (the caller refills via [`load`](Self::load) or leaves the slot idle);
    /// a stuck lane branches and a dead lane backtracks **in place**, with no callback.
    /// The stepping primitive a two-warp host drives; [`run_stream`](Self::run_stream)
    /// is this in a loop with immediate refill.
    pub fn step<F: FnMut(usize, bool)>(&mut self, mut on_verdict: F) {
        let active_mask = M::from_array(self.active);
        if !active_mask.any() {
            return;
        }
        dstat_add(1, 1);
        dstat_add(3, active_mask.to_bitmask().count_ones() as u64);
        let (changed, dead, solved) = warp_pass(&mut self.r, &mut self.unsolved, active_mask);

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
                if !self.backtrack(l) {
                    verdict = Some(false); // tree exhausted: no alternate completion
                }
            } else {
                self.branch(l);
                self.branched[l] = true;
            }
            if let Some(v) = verdict {
                #[cfg(feature = "count")]
                {
                    pstat_bump(if self.branched[l] { 2 } else if v { 0 } else { 1 });
                    // SAFETY: single-threaded lab.
                    unsafe { LAST_BRANCHED[l] = self.branched[l] };
                }
                self.active[l] = false;
                on_verdict(l, v);
            }
        }
    }

    /// Drive the warp as a **streaming** existence prober: rather than consuming a
    /// fixed probe slice, each freed SIMD lane is refilled by calling
    /// `next(slot, verdict)`. The callback receives the just-finished probe's verdict
    /// (`true` = an alternate completion exists, i.e. the strip is non-unique;
    /// `None` on the initial fill) and returns the next [`Probe`] for that lane, or
    /// `None` when the lane has no more work.
    ///
    /// One loop iteration = one [`warp_pass`] over the active lanes + per-lane
    /// service: a solved lane is a verdict (`true`); a dead lane backtracks (or, tree
    /// exhausted, is verdict `false`); a stalled lane (a full pass left it unchanged,
    /// still unsolved) branches; a still-changing lane keeps propagating in place. A
    /// lane reaching a verdict refills via `next` (or frees its slot).
    ///
    /// Because `next` *produces the next query on demand* — the host advances the
    /// owning attempt past the just-resolved gate — W=8 live attempts hold the warp
    /// at ~100% utilization with no depth tunable: the killswitch tape, generated
    /// lazily. (The batch [`Self::resolve`] is the same loop fed from a slice.)
    pub fn run_stream<F>(&mut self, mut next: F)
    where
        F: FnMut(usize, Option<bool>) -> Option<Probe>,
    {
        self.active = [false; LANES];
        self.branched = [false; LANES];
        for s in &mut self.stacks {
            s.clear();
        }
        // Initial fill: ask for one probe per lane.
        for l in 0..LANES {
            if let Some(p) = next(l, None) {
                self.load(l, &p);
            }
        }
        // One pass + service per iteration, refilling each terminated slot on demand.
        // Terminations are collected from `step` (which can't re-borrow `self` to call
        // `next`) then refilled here — the lanes are independent, so deferring the refill
        // to after the pass is byte-identical to the inline refill it replaced.
        while self.any_active() {
            let mut ts = [0usize; LANES];
            let mut tv = [false; LANES];
            let mut tn = 0usize;
            self.step(|l, v| {
                ts[tn] = l;
                tv[tn] = v;
                tn += 1;
            });
            for i in 0..tn {
                let l = ts[i];
                if let Some(p) = next(l, Some(tv[i])) {
                    self.load(l, &p);
                }
            }
        }
    }

    /// Resolve a fixed batch of probes, writing `out[i]` = "an alternate completion
    /// exists for `probes[i]`". A thin [`Self::run_stream`] wrapper that feeds
    /// `probes` in order — used by the isolated `probebench` and the diagnostics.
    /// `out.len()` must be `>= probes.len()`.
    pub fn resolve(&mut self, probes: &[Probe], out: &mut [bool]) {
        let mut idx = 0usize;
        let mut lane_probe = [0usize; LANES];
        self.run_stream(|slot, verdict| {
            if let Some(v) = verdict {
                out[lane_probe[slot]] = v;
            }
            if idx < probes.len() {
                lane_probe[slot] = idx;
                idx += 1;
                Some(probes[idx - 1])
            } else {
                None
            }
        });
    }

    /// Branch lane `l`: pick a cell (a bivalue cell if any, else any unsolved one),
    /// push a frame saving the pre-branch board and the cell's untried digits, and
    /// assign its first candidate (the next pass places it via the smear).
    fn branch(&mut self, l: usize) {
        dstat_add(2, 1);
        let (sr, su) = snapshot_lane(&self.r, &self.unsolved, l);
        let (cell, mask) = branch_cell(&sr, &su);
        let d = mask.trailing_zeros() as usize;
        self.stacks[l].push(Frame { r: sr, unsolved: su, cell: cell as u32, remaining: mask & (mask - 1) });
        assign(&mut self.r, l, cell, d);
    }

    /// Backtrack lane `l` to the shallowest frame with an untried digit: restore
    /// that frame's board and assign its next candidate. Returns false if the whole
    /// search tree is exhausted (no completion).
    fn backtrack(&mut self, l: usize) -> bool {
        loop {
            // Scope the frame borrow so the board restore can take `&mut self.r`.
            let (cell, d, sr, su) = {
                let Some(frame) = self.stacks[l].last_mut() else {
                    return false;
                };
                if frame.remaining == 0 {
                    self.stacks[l].pop();
                    continue;
                }
                let cell = frame.cell as usize;
                let d = frame.remaining.trailing_zeros() as usize;
                frame.remaining &= frame.remaining - 1;
                (cell, d, frame.r, frame.unsolved)
            };
            restore_lane(&mut self.r, &mut self.unsolved, l, &sr, &su);
            assign(&mut self.r, l, cell, d);
            return true;
        }
    }
}

/// Branch lane `l` on an **externally-owned** frame stack (the unified-warp engine in
/// [`crate::solve::simt`] keeps the prober's per-lane stacks itself rather than going
/// through [`PackedProber`]). Pick a cell, push the pre-branch board + untried digits,
/// assign the first candidate. Mirror of [`PackedProber::branch`], stack passed in.
pub(crate) fn branch_lane(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], stack: &mut Vec<Frame>, l: usize) {
    dstat_add(2, 1);
    let (sr, su) = snapshot_lane(r, unsolved, l);
    let (cell, mask) = branch_cell(&sr, &su);
    let d = mask.trailing_zeros() as usize;
    stack.push(Frame { r: sr, unsolved: su, cell: cell as u32, remaining: mask & (mask - 1) });
    assign(r, l, cell, d);
}

/// Backtrack lane `l` on an externally-owned frame stack — mirror of
/// [`PackedProber::backtrack`]. Returns false if the tree is exhausted (no completion).
pub(crate) fn backtrack_lane(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], stack: &mut Vec<Frame>, l: usize) -> bool {
    loop {
        let (cell, d, sr, su) = {
            let Some(frame) = stack.last_mut() else {
                return false;
            };
            if frame.remaining == 0 {
                stack.pop();
                continue;
            }
            let cell = frame.cell as usize;
            let d = frame.remaining.trailing_zeros() as usize;
            frame.remaining &= frame.remaining - 1;
            (cell, d, frame.r, frame.unsolved)
        };
        restore_lane(r, unsolved, l, &sr, &su);
        assign(r, l, cell, d);
        return true;
    }
}

/// Pick the branch cell + its candidate-digit mask from a lane's **already-snapshotted**
/// scalar board (`sr`/`su` from [`snapshot_lane`]): prefer a bivalue cell (exactly two
/// candidates) to keep the branching factor at 2, else the lowest unsolved cell. A stalled
/// lane has no naked single, so the pick has >= 2 digits.
///
/// Reads the scalar snapshot rather than re-extracting lane `l` out of the SoA warp: every
/// caller snapshots anyway (to push the pre-branch frame), so taking the pick off that copy
/// saves ~36 SIMD lane-gathers per branch — the fold over all 9x3 bands plus the mask scan,
/// which previously pulled each `r[d][b]`/`unsolved[b]` element out a second time.
#[cfg_attr(feature = "profiling", inline(never))]
pub(crate) fn branch_cell(sr: &[[u32; 3]; 9], su: &[u32; 3]) -> (usize, u16) {
    let mut ones = [0u32; 3];
    let mut twos = [0u32; 3];
    let mut threes = [0u32; 3];
    for d in 0..9 {
        for b in 0..3 {
            let x = sr[d][b];
            threes[b] |= twos[b] & x;
            twos[b] |= ones[b] & x;
            ones[b] |= x;
        }
    }
    let mut cell = usize::MAX;
    for b in 0..3 {
        let biv = su[b] & twos[b] & !threes[b];
        if biv != 0 {
            cell = rm_cell(b, biv.trailing_zeros());
            break;
        }
    }
    if cell == usize::MAX {
        for b in 0..3 {
            if su[b] != 0 {
                cell = rm_cell(b, su[b].trailing_zeros());
                break;
            }
        }
    }
    let cl = rm_lane(cell);
    let cbit = 1u32 << rm_bit(cell);
    let mut mask = 0u16;
    for d in 0..9 {
        if sr[d][cl] & cbit != 0 {
            mask |= 1 << d;
        }
    }
    (cell, mask)
}
