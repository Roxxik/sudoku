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
use crate::repr::{Branchable, Digit, FlatGridMask, Marks, SearchState};
use crate::sieve::Sieve;

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

/// Drive a [`SearchState`] to a propagation fixpoint in place before the search
/// branches: place every forced cell so the branch only ever fires on a stuck board.
/// The swap point — the flat reference does naked singles; the banded packing adds the
/// fused hidden-single sweep.
pub trait Propagate: Branchable {
    /// Place forced cells until none remain, returning the resulting [`Fixpoint`]. The
    /// caller branches only on [`Fixpoint::Stuck`]; `Solved`/`Dead` are terminal, so it
    /// skips the branch scan on them — the verdict the drain already computed, rather
    /// than a second pass to rediscover it.
    fn propagate(state: &mut SearchState<Self>) -> Fixpoint;
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
fn drain_naked_singles<M: Branchable>(state: &mut SearchState<M>) -> Fixpoint {
    loop {
        // The raw (unmasked) sieve: a decided cell's stale bits can inflate its tier,
        // but `& unsolved` below drops it — so the per-digit `& unsolved` `compute`
        // would pay is redundant here (bb's prober sieve).
        let sieve = Sieve::<M, 2>::compute_raw(state.candidates());
        let unsolved = state.unsolved();
        let singles = unsolved & sieve.exactly(1);
        if !singles.any() {
            return if !unsolved.any() {
                Fixpoint::Solved
            } else if (unsolved & sieve.dead()).any() {
                Fixpoint::Dead
            } else {
                Fixpoint::Stuck
            };
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
    fn propagate(state: &mut SearchState<Self>) -> Fixpoint {
        drain_naked_singles(state) // no in-lane units to sweep — naked singles are all
    }
}

impl Propagate for Bands<RowMajor> {
    /// Naked singles drained, then the fused row/box hidden-single sweep, looping
    /// until neither fires — bb's `propagate` structure on the new representation.
    fn propagate(state: &mut SearchState<Self>) -> Fixpoint {
        crate::bb::band_ctr_inc(0); // propagate-calls (no-op without feature "count")
        loop {
            // Solved/dead boards can't yield a hidden single — skip the sweep, as bb's
            // naked loop returns Solved/Contradiction before its band update.
            match drain_naked_singles(state) {
                Fixpoint::Stuck => {}
                terminal => return terminal,
            }
            crate::bb::band_ctr_inc(1); // band-pass
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
fn lone(unit: usize) -> Option<usize> {
    let s = SINGLE9[unit];
    (s != 0xFF).then_some(s as usize)
}

/// One pass of bb's fused row-major hidden-single sweep: for each band and digit, read
/// the band's live candidates once and place any digit forced into a single cell of a
/// row or box (the units in-lane in this view). Columns straddle bands and are reached
/// by branching, never swept. Returns whether any placement was made, so the caller
/// loops it against [`drain_naked_singles`] to a joint fixpoint.
#[inline(always)]
fn band_hidden_singles(state: &mut SearchState<Bands<RowMajor>>) -> bool {
    let mut changed = false;
    for b in 0..3 {
        for di in 0..9 {
            let digit = Digit::from_index(di);
            // The digit's live candidates in band b, re-read after each placement (a
            // place in this band can create another hidden single later in the scan).
            let mut band = (state.candidates()[digit] & state.unsolved()).band(b);
            for line in 0..3 {
                if let Some(slot) = lone(band.line(line)) {
                    state.place(RowMajor::cell_at(b, Band::line_pos(line, slot)), digit);
                    changed = true;
                    band = (state.candidates()[digit] & state.unsolved()).band(b);
                }
            }
            for k in 0..3 {
                if let Some(slot) = lone(band.box_unit(k)) {
                    state.place(RowMajor::cell_at(b, Band::box_pos(k, slot)), digit);
                    changed = true;
                    band = (state.candidates()[digit] & state.unsolved()).band(b);
                }
            }
        }
    }
    changed
}
