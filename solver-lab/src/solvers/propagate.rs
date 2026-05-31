//! Variant `propagate`: same search as [`simd_dense`](super::simd_dense)
//! (naked-single cascade + SIMD-MRV branching) but with **incremental
//! propagation** instead of a full rescan per placement.
//!
//! The baseline re-scans all 96 lanes after every single it places, so a
//! cascade of k forced singles costs k full scans — quadratic in the cascade
//! length, and the cascade is where existence solves spend most of their time.
//!
//! Here, placing a digit touches only the cell's 20 peers; a peer that drops to
//! one candidate is pushed on a work stack and placed in turn (a contradiction
//! — a peer dropping to zero — aborts immediately). The expensive SIMD MRV scan
//! runs only when the cascade drains and we must pick a branch cell. Branches
//! are rare relative to singles, so the full scans nearly vanish.
//!
//! Everything else matches the baseline: dense `[u16; 96]` candidate array,
//! FILLED sentinel, clone-per-branch. The MRV branch order is identical, so
//! this variant walks the same search tree and must return identical answers.

use crate::grid::{ALL_DIGITS, Board, CELLS, PEERS};
use crate::solvers::UniqProber;
use std::simd::cmp::{SimdOrd, SimdPartialEq};
use std::simd::num::SimdUint;
use std::simd::{Mask, Select, Simd};

const LANES: usize = 16;
const PADDED: usize = 96;
const FILLED: u16 = 0x200;

const LANE_INDICES: [u16; LANES] = {
    let mut a = [0u16; LANES];
    let mut i = 0;
    while i < LANES {
        a[i] = i as u16;
        i += 1;
    }
    a
};

#[derive(Clone)]
struct CpSolver {
    cand: [u16; PADDED],
}

impl CpSolver {
    fn from_board_candidates(b: &Board) -> Self {
        let mut s = CpSolver {
            cand: [FILLED; PADDED],
        };
        for i in 0..CELLS {
            if b.cell(i) == 0 {
                s.cand[i] = b.candidates(i);
            }
        }
        s
    }

    /// Place `d` at cell `i` and clear it from every peer. Any peer that drops
    /// to a single candidate is pushed on the stack (a new naked single); a peer
    /// dropping to zero is a contradiction. Returns false on contradiction.
    #[inline]
    fn place_one(&mut self, i: usize, d: u8, stack: &mut [u8; CELLS], top: &mut usize) -> bool {
        let clear = !(1u16 << (d as u16 - 1));
        // SAFETY: `i` and every PEERS[i] entry are valid cell indices (0..81),
        // `cand` has 96 slots, and `stack` has CELLS slots — `top` never exceeds
        // CELLS because each cell is pushed at most once (a cell only crosses
        // the 2->1 candidate boundary a single time; candidates only shrink).
        unsafe {
            *self.cand.get_unchecked_mut(i) = FILLED;
            for &p in PEERS.get_unchecked(i) {
                let before = *self.cand.get_unchecked(p);
                if before & FILLED != 0 {
                    continue; // filled peer (sentinel) — nothing to clear
                }
                let after = before & clear;
                if after == before {
                    continue; // `d` wasn't a candidate here
                }
                *self.cand.get_unchecked_mut(p) = after;
                let pc = after.count_ones(); // digit bits only (no sentinel set)
                if pc == 0 {
                    return false; // contradiction
                }
                if pc == 1 {
                    *stack.get_unchecked_mut(*top) = p as u8;
                    *top += 1;
                }
            }
        }
        true
    }

    /// Drain the work stack of forced naked singles. Returns false on
    /// contradiction.
    #[inline]
    fn drain(&mut self, stack: &mut [u8; CELLS], mut top: usize) -> bool {
        while top > 0 {
            top -= 1;
            let c = stack[top] as usize;
            let m = self.cand[c];
            if m & FILLED != 0 {
                continue; // already placed via another peer
            }
            let bits = m & ALL_DIGITS;
            match bits.count_ones() {
                0 => return false,         // a peer placement emptied it
                1 => {}                    // still forced — place below
                _ => continue,             // no longer a single (defensive)
            }
            let d = bits.trailing_zeros() as u8 + 1;
            if !self.place_one(c, d, stack, &mut top) {
                return false;
            }
        }
        true
    }

    /// Place `d` at `i` and run the resulting naked-single cascade to a fixpoint.
    #[inline]
    fn place_and_propagate(&mut self, i: usize, d: u8) -> bool {
        let mut stack = [0u8; CELLS];
        let mut top = 0usize;
        if !self.place_one(i, d, &mut stack, &mut top) {
            return false;
        }
        self.drain(&mut stack, top)
    }

    /// Probe entry point: place `d` at `i`, then propagate `i`'s cascade
    /// *together with* every pre-existing naked single in the base, to a
    /// fixpoint, and search.
    ///
    /// The base is intentionally NOT pre-propagated. The strip loop derives the
    /// alternates from the *un-propagated* candidates, so pre-filling a forced
    /// single could place `d` itself in a peer of `i`; `place_one` skips filled
    /// peers, which would then silently mask that conflict and wrongly report a
    /// solution. Placing `i=d` first (always consistent with the givens, since
    /// `d` is a candidate of `i`) and only then propagating avoids that — the
    /// only filled cells before the placement are givens, none of which hold `d`.
    fn solve_from(&mut self, i: usize, d: u8) -> bool {
        let mut stack = [0u8; CELLS];
        let mut top = 0usize;
        // Seed with every pre-existing naked single (the base is raw).
        for c in 0..CELLS {
            let m = self.cand[c];
            if m & FILLED == 0 && (m & ALL_DIGITS).count_ones() == 1 {
                stack[top] = c as u8;
                top += 1;
            }
        }
        if !self.place_one(i, d, &mut stack, &mut top) {
            return false;
        }
        if !self.drain(&mut stack, top) {
            return false;
        }
        self.solve_first()
    }

    /// SIMD MRV scan over all cells: `(best_cell, best_count, best_mask)`.
    /// `best_cell == usize::MAX` => solved. Identical to the baseline's scan;
    /// here it is only ever called when the cascade has drained, so the winning
    /// count is always >= 2 (or "solved").
    #[inline]
    fn scan(&self) -> (usize, u16) {
        let filled_bit = Simd::<u16, LANES>::splat(FILLED);
        let penalty = Simd::<u16, LANES>::splat(100);
        let zero = Simd::<u16, LANES>::splat(0);
        let seven = Simd::<u16, LANES>::splat(7);
        let lane_idx = Simd::<u16, LANES>::from_array(LANE_INDICES);
        let mut acc = Simd::<u16, LANES>::splat(u16::MAX);
        for k in 0..PADDED / LANES {
            // SAFETY: k*LANES + LANES <= PADDED.
            let v = Simd::<u16, LANES>::from_slice(unsafe {
                self.cand.get_unchecked(k * LANES..k * LANES + LANES)
            });
            let pc = v.count_ones();
            let is_filled: Mask<i16, LANES> = (v & filled_bit).simd_ne(zero);
            let s = is_filled.select(penalty, pc);
            let idx = lane_idx + Simd::<u16, LANES>::splat((k * LANES) as u16);
            acc = acc.simd_min((s << seven) | idx);
        }
        let best = acc.reduce_min();
        let best_count = best >> 7;
        if best_count >= 100 {
            return (usize::MAX, 0); // every cell filled — solved
        }
        let i = (best & 0x7F) as usize;
        // SAFETY: i < 81 — it won a real (non-penalty) score.
        let mask = unsafe { *self.cand.get_unchecked(i) } & ALL_DIGITS;
        (i, mask)
    }

    /// True if the (already-propagated) grid has at least one completion.
    fn solve_first(&mut self) -> bool {
        loop {
            let (best_cell, best_mask) = self.scan();
            if best_cell == usize::MAX {
                return true; // solved
            }
            // best_mask has >= 2 bits here (cascade drained), so the inner loop
            // runs at least twice and reaches the `m == 0` tail.
            let mut m = best_mask;
            loop {
                let d = m.trailing_zeros() as u8 + 1;
                m &= m - 1;
                if m == 0 {
                    // Last alternative: place in self (no clone) and re-scan.
                    return self.place_and_propagate(best_cell, d) && self.solve_first();
                }
                let mut child = self.clone();
                if child.place_and_propagate(best_cell, d) && child.solve_first() {
                    return true;
                }
            }
        }
    }
}

/// `propagate` existence probe: build + stabilise the base once, clone +
/// place-with-propagation per alternate digit.
pub struct Probe {
    base: CpSolver,
}

impl UniqProber for Probe {
    const NAME: &'static str = "propagate";

    fn from_board(board: &Board) -> Self {
        Probe {
            base: CpSolver::from_board_candidates(board),
        }
    }

    #[inline]
    fn has_solution_with(&mut self, i: usize, d: u8) -> bool {
        let mut s = self.base.clone();
        s.solve_from(i, d)
    }
}
