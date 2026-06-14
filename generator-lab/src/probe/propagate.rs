//! Propagation to a fixpoint before the search branches — the prober's pruning,
//! behind a per-packing swap point.
//!
//! The existence/uniqueness search is correct without any propagation (completeness
//! comes from the branch), so propagation is *pure pruning*: it never changes the
//! completion count or the dead/solved verdict, only the work to reach it. That frees
//! each packing to place any sound subset of forced cells before branching. Two
//! things make this worth a trait rather than one generic function:
//!
//! - Naked singles are packing-agnostic — the [`Sieve`] surfaces them on any
//!   [`Branchable`] board — so [`drain_naked_singles`] is shared by every impl.
//! - Hidden singles are not: a fast sweep reads a whole unit at once, which only the
//!   banded layout puts in-lane. So the flat reference stops at naked singles, while
//!   the banded production packing adds bb's fused per-band hidden-single sweep
//!   ([`band_hidden_singles`]) — the composable all-27-units technique is far too slow
//!   for the hot path (it is why the `Singles` prober trails by ~24x).
//!
//! Driving the board to a fixpoint here is what makes [`Bivalue`](crate::scan::Bivalue)
//! — bb's branch rule — viable: a bivalue branch only holds the factor at two once the
//! board is genuinely stuck.

use crate::repr::banded::{Band, Banding, Bands, RowMajor};
use crate::repr::{Branchable, Digit, FlatGridMask, Marks, SolverState};
use crate::scan::sieve::Sieve;

/// The state of a board once propagation can place no more forced cells.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fixpoint {
    /// Every cell decided — a completion.
    Solved,
    /// A live cell has no candidate left — this branch is dead.
    Dead,
    /// Neither: every unsolved cell has >= 2 candidates, so the search must branch.
    Stuck,
}

/// Drive a [`SolverState`] to a propagation fixpoint in place before the search
/// branches: place every forced cell so the branch only ever fires on a stuck board.
/// The swap point — the flat reference does naked singles; the banded packing adds the
/// fused hidden-single sweep.
pub trait Propagate: Branchable {
    /// Place forced cells until none remain, returning the resulting [`Fixpoint`]. The
    /// caller branches only on [`Fixpoint::Stuck`]; `Solved`/`Dead` are terminal, so it
    /// skips the branch scan on them — the verdict the drain already computed, rather
    /// than a second pass to rediscover it.
    fn propagate(state: &mut SolverState<Self>) -> Fixpoint;
}

/// Place every naked single (one-candidate cell) in place, looping to a fixpoint —
/// the packing-agnostic half of propagation, shared by every [`Propagate`] impl. Each
/// placement cascades (a peer it forbids may itself fall to one candidate), so this
/// drains the whole forced chain a scan would otherwise branch through one clone at a
/// time.
/// Classifies the board once no naked single remains: [`Solved`](Fixpoint::Solved) /
/// [`Dead`](Fixpoint::Dead) are terminal (the caller skips the hidden-single sweep —
/// it can place nothing on either, as bb returns `Solved`/`Contradiction` from its
/// naked loop), [`Stuck`](Fixpoint::Stuck) means every unsolved cell has >= 2
/// candidates so the sweep may still fire.
#[inline(always)]
pub(crate) fn drain_naked_singles<M: Branchable>(state: &mut SolverState<M>) -> Fixpoint {
    loop {
        // The raw (unmasked) sieve: a decided cell's stale bits can inflate its tier,
        // but `& unsolved` below drops it — so the per-digit `& unsolved` `compute`
        // would pay is redundant here (bb's prober sieve).
        let sieve = Sieve::<M, 2>::compute_raw(state.candidates());
        let unsolved = state.unsolved();
        // Classify dead/solved FIRST, every pass (bb's order). The obvious structure —
        // compute `singles`, and only classify dead/solved once it empties — makes the
        // compiler lay out the hot fixpoint loop with ~7% more *taken* branches (816M ->
        // 871M over a 15k-seed run). On Zen each taken branch closes an op-cache fetch
        // window, so that throttles micro-op delivery and makes the prober frontend-bound
        // (~12% slower than bb). Hoisting the dead/solved checks above the single placement
        // restores bb's branch layout — gap closed (in fact ~3% faster than bb), verdict
        // and fingerprint unchanged.
        if (unsolved & sieve.dead()).any() {
            return Fixpoint::Dead;
        }
        if !unsolved.any() {
            return Fixpoint::Solved;
        }
        let singles = unsolved & sieve.exactly(1);
        if !singles.any() {
            return Fixpoint::Stuck;
        }
        // Group this pass's singles by their lone digit — `singles & board[d]` is the
        // singles whose candidate is `d` (a single sits in exactly one digit's board) —
        // and place each group in one batched op, bb's `place_singles`. Reading the
        // digit off the board this way replaces the per-cell 9-board candidate search
        // the one-at-a-time placement paid. A placement only removes candidates, so no
        // new single appears mid-pass; the next pass's sieve surfaces those.
        for di in 0..9 {
            let d = Digit::from_index(di);
            let group = singles & state.candidates()[d];
            if group.any() {
                state.place_single_group(d, group);
            }
        }
    }
}

impl Propagate for FlatGridMask {
    /// The flat reference has no in-lane units, so it stops at naked singles — still
    /// correct (the branch reaches every completion), just less pruning than banded.
    #[inline]
    fn propagate(state: &mut SolverState<Self>) -> Fixpoint {
        drain_naked_singles(state) // no in-lane units to sweep — naked singles are all
    }
}

impl Propagate for Bands<RowMajor> {
    /// Naked singles drained, then the fused row/box hidden-single sweep, looping
    /// until neither fires — bb's `propagate` structure on the new representation.
    fn propagate(state: &mut SolverState<Self>) -> Fixpoint {
        super::band_ctr_inc(0); // propagate-calls (no-op without feature "count")
        loop {
            // Solved/dead boards can't yield a hidden single — skip the sweep, as bb's
            // naked loop returns Solved/Contradiction before its band update.
            match drain_naked_singles(state) {
                Fixpoint::Stuck => {}
                terminal => return terminal,
            }
            super::band_ctr_inc(1); // band-pass
            if !band_hidden_singles(state) {
                return Fixpoint::Stuck;
            }
        }
    }
}

/// Hidden-single lookup for a 9-cell unit reduced to a 9-bit candidate mask: the lone
/// set bit's index if exactly one is set, else `None`. A row, column, or box collapses
/// to nine bits ([`Band::line`]/[`Band::box_unit`]), so detecting *and* locating a
/// hidden single is one table load — no per-cell `exactly_one` + scan.
const SINGLE9: [u8; 512] = {
    let mut t = [0xFFu8; 512];
    let mut v = 1usize;
    while v < 512 {
        if v & (v - 1) == 0 {
            t[v] = v.trailing_zeros() as u8;
        }
        v += 1;
    }
    t
};

/// The lone candidate's slot in a 9-bit unit mask, or `None` if the unit has zero or
/// more than one candidate.
#[inline]
pub(crate) fn lone(unit: usize) -> Option<usize> {
    let s = SINGLE9[unit];
    (s != 0xFF).then_some(s as usize)
}

/// One pass of bb's fused row-major hidden-single sweep: for each digit and band, read
/// the band's live candidates once and place any digit forced into a single cell of a
/// row or box (the units in-lane in this view). Columns straddle bands and are reached
/// by branching, never swept. Returns whether any placement was made, so the caller
/// loops it against [`drain_naked_singles`] to a joint fixpoint.
///
/// Each of the six in-lane units is read in place via [`Band::unit_single`]: one AND with
/// a [`Band::UNIT_MASKS`] constant plus a power-of-two test, the set bit's index serving
/// directly as the band position. This is the prober's hot path (~a quarter of scalar
/// generation), and the in-place read replaces the per-unit `box_unit` gather, 512-byte
/// `SINGLE9` table load, and slot->position remap the lookup form paid.
///
/// Digit-outer: the live set `candidates[d] & unsolved` is one SIMD AND across all three
/// bands, so compute it once per digit and extract each band's lane, rather than re-AND-ing
/// per (band, digit) (~18 fewer 16-byte SIMD loads + ANDs per no-placement sweep). A
/// placement refreshes it (peers can cross bands). The reorder is fixpoint-confluent (hidden
/// singles are monotone forced placements), so the propagation fixpoint — the only board the
/// search ever branches on — is identical; mirrors the fused solver's `band_update_rm`.
///
/// Band-unrolled: the three bands are swept by an explicit [`sweep_band`] each (`B` a
/// constant), not a `for b in 0..3`. A runtime band index makes `live_all.band(b)` read
/// through [`as_array`](std::simd::Simd::as_array), which stores the whole vector to the
/// stack and reloads the lane every band — and the reload feeds the hot `unit_single`
/// AND/popcnt off a store-forward. With a const index ([`Bands::band_at`]) the extract is a
/// register `vpextrd`, no stack round-trip. The prober is memory-port-bound, not op-bound
/// (SDE icount points the wrong way), so killing the per-band store + reload is the win:
/// interleaved A/B (proberab harness) -2.7% e2e on train+drill(naked-pair), fp-identical,
/// over the already-digit-outer baseline. Same physics as the digit-outer hoist above.
#[inline(always)]
pub(crate) fn band_hidden_singles(state: &mut SolverState<Bands<RowMajor>>) -> bool {
    let mut changed = false;
    for di in 0..9 {
        let digit = Digit::from_index(di);
        // The digit's live candidates across all three bands, re-read after each placement
        // (a place can create another hidden single, in this band or another).
        let mut live_all = state.candidates()[digit] & state.unsolved();
        changed |= sweep_band::<0>(state, &mut live_all, digit);
        changed |= sweep_band::<1>(state, &mut live_all, digit);
        changed |= sweep_band::<2>(state, &mut live_all, digit);
    }
    changed
}

/// One band's six in-lane units swept for a hidden single, band index `B` a constant.
/// Refreshes the shared `live_all` after each placement (a place can create a hidden
/// single in this or a later band), so the caller's next `sweep_band` sees it.
#[inline(always)]
fn sweep_band<const B: usize>(
    state: &mut SolverState<Bands<RowMajor>>,
    live_all: &mut Bands<RowMajor>,
    digit: Digit,
) -> bool {
    let mut changed = false;
    let mut band = live_all.band_at::<B>();
    for &mask in &Band::UNIT_MASKS {
        if let Some(pos) = band.unit_single(mask) {
            state.place(RowMajor::cell_at(B, pos), digit);
            changed = true;
            *live_all = state.candidates()[digit] & state.unsolved();
            band = live_all.band_at::<B>();
        }
    }
    changed
}
