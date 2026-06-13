//! Packed SoA prober on the `repr` layer: the [`crate::generate::warp_host`] batch
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

// `pub` (not crate): they appear in the [`crate::generate::warp_host::Engine`]
// trait's method signatures (the boards a job's scalar service mutates).
pub type V = Simd<u32, LANES>;
pub type M = Mask<i32, LANES>;
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
/// Compiles to a vectorized popcount on AVX-512
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

// --- packed-DFS prober metrics (feature = "count") ----------------------------
// [0] probes, [1] warp passes (ticks), [2] branch nodes, [3] active-lane-sum over
// ticks. utilization = active_lane_sum / (LANES * ticks); branches/probe and
// passes/probe size the node inflation vs the scalar prober (`infl`). `dstat_add(i, v)`
// tallies; it is a no-op when the feature is off, so its call sites need no gating.
counter_block!(DSTAT: 4, inc = dstat_inc, add = dstat_add, snapshot = dstat_snapshot, reset = dstat_reset);

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

/// Snapshot lane `l` directly into caller-owned `sr`/`su` slots — the same strided
/// 30-`u32` read as [`snapshot_lane`], but written in place rather than returned by
/// value. Lets [`branch_lane`] land the snapshot straight in its pushed [`Frame`]
/// instead of into a local that is then copied a second time into the stack slot.
#[inline]
pub(crate) fn snapshot_lane_into(
    r: &[[V; 3]; 9],
    unsolved: &[V; 3],
    l: usize,
    sr: &mut [[u32; 3]; 9],
    su: &mut [u32; 3],
) {
    for d in 0..9 {
        for b in 0..3 {
            sr[d][b] = r[d][b].as_array()[l];
        }
    }
    for b in 0..3 {
        su[b] = unsolved[b].as_array()[l];
    }
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

impl Frame {
    /// Fold the frame's board + scalar fields into one checksum word — the frame-fusion
    /// microbench's equivalence pin (all `branch_lane` variants must agree) and DCE guard.
    /// Lab-only; the fields are private to this module, so the bench reads them through here.
    #[inline]
    pub(crate) fn fold_hash(&self) -> u64 {
        let mut h = (self.cell as u64) ^ ((self.remaining as u64) << 40);
        for d in 0..9 {
            for b in 0..3 {
                h ^= (self.r[d][b] as u64).rotate_left((d * 3 + b) as u32);
            }
        }
        for b in 0..3 {
            h ^= (self.unsolved[b] as u64) << (b * 9);
        }
        h
    }
}

/// Branch lane `l` on an **externally-owned** frame stack (the unified-warp engine in
/// [`crate::solve::simt`] keeps the prober's per-lane stacks itself rather than going
/// through [`PackedProber`]). Pick a cell, push the pre-branch board + untried digits,
/// assign the first candidate. Mirror of [`PackedProber::branch`], stack passed in.
///
/// The pre-branch board is snapshotted **directly into the pushed frame** rather than into
/// a local that is then copied a second time. The strided SoA read ([`snapshot_lane_into`])
/// is the design's irreducible per-branch clone; the old `let (sr, su) = snapshot_lane(..);
/// push(Frame { r: sr, .. })` form copied those 31 words a second time, local -> `Vec`
/// slot. Fusing it removes ~111 instructions per branch node (dominated by 82 redundant
/// memory writes — `examples/framefusebench` measures it under Intel SDE `-mix`). The only
/// residue is two dead AVX-512 zero-stores from the placeholder push that LLVM keeps across
/// the call; the second copy itself is gone.
pub(crate) fn branch_lane(r: &mut [[V; 3]; 9], unsolved: &mut [V; 3], stack: &mut Vec<Frame>, l: usize) {
    dstat_add(2, 1);
    stack.push(Frame { r: [[0; 3]; 9], unsolved: [0; 3], cell: 0, remaining: 0 });
    let frame = stack.last_mut().unwrap();
    snapshot_lane_into(r, unsolved, l, &mut frame.r, &mut frame.unsolved);
    let (cell, mask) = branch_cell(&frame.r, &frame.unsolved);
    frame.cell = cell as u32;
    frame.remaining = mask & (mask - 1);
    let d = mask.trailing_zeros() as usize;
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
