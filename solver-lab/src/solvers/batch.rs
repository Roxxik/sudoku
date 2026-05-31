//! Variant `batch`: keep [`simd_dense`](super::simd_dense)'s dense `[u16; 96]`
//! representation and its branch-free SIMD `place` (the reason it wins on
//! native), but kill the documented quadratic.
//!
//! The baseline places exactly ONE forced single per `scan()`, so a cascade of
//! k naked singles costs k full-board scans. [`propagate`](super::propagate)
//! removes the rescans, but pays for it with a branchy scalar peer-walk per
//! placement — cheap on wasm (slow SIMD) yet a net loss on native (fast SIMD).
//!
//! `batch` takes the third road: one SIMD scan extracts *every* currently-forced
//! single at once (a 96-bit lane mask), then places them all with the same
//! branch-free `place` the baseline uses, and only then rescans. So a k-single
//! cascade collapses from k scans to roughly the number of *waves* of singles
//! (usually 1-3), while every placement stays the cheap branch-free AND-sweep.
//! No scalar peer loop, no work stack — just fewer scans over the identical
//! representation. The branch order (SIMD MRV) is unchanged, so the search tree
//! and every answer match the baseline.

use crate::grid::{ALL_DIGITS, Board, CELLS, PEERS};
use crate::solvers::UniqProber;
use std::simd::cmp::{SimdOrd, SimdPartialEq};
use std::simd::num::SimdUint;
use std::simd::{Mask, Select, Simd};

const LANES: usize = 16;
const PADDED: usize = 96; // 6 * LANES, >= 81
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
struct FastSolver {
    cand: [u16; PADDED],
}

impl FastSolver {
    fn from_board_candidates(b: &Board) -> Self {
        let mut s = FastSolver {
            cand: [FILLED; PADDED],
        };
        for i in 0..CELLS {
            if b.cell(i) == 0 {
                s.cand[i] = b.candidates(i);
            }
        }
        s
    }

    /// Branch-free placement, identical to the baseline: cell `i` becomes
    /// FILLED and digit `d`'s bit is cleared from all 20 peers.
    #[inline]
    fn place(&mut self, i: usize, d: u8) {
        crate::counters::bump_placements();
        let clear = !(1u16 << (d as u16 - 1));
        // SAFETY: `i` and every `PEERS[i]` entry are valid cell indices (0..81)
        // and `cand` has 96 slots, so all accesses are in bounds.
        unsafe {
            *self.cand.get_unchecked_mut(i) = FILLED;
            for &p in PEERS.get_unchecked(i) {
                *self.cand.get_unchecked_mut(p) &= clear;
            }
        }
    }

    /// One SIMD pass over all 96 lanes returning everything the search needs:
    /// `(mrv_cell, mrv_count, mrv_mask, singles)`.
    ///
    /// - `mrv_count == 0` => dead end (some empty cell has no candidate).
    /// - `mrv_cell == usize::MAX` (mrv_count >= 100) => solved (all filled).
    /// - otherwise `mrv_cell`/`mrv_mask` is the minimum-candidate empty cell.
    /// - `singles` is a 96-bit mask, bit `j` set iff empty cell `j` has exactly
    ///   one candidate — the whole forced-single wave, extracted in this pass.
    #[inline]
    fn scan(&self) -> (usize, u32, u16, u128) {
        crate::counters::bump_scans();
        let filled_bit = Simd::<u16, LANES>::splat(FILLED);
        let digits = Simd::<u16, LANES>::splat(ALL_DIGITS);
        let penalty = Simd::<u16, LANES>::splat(100);
        let zero = Simd::<u16, LANES>::splat(0);
        let one = Simd::<u16, LANES>::splat(1);
        let seven = Simd::<u16, LANES>::splat(7);
        let lane_idx = Simd::<u16, LANES>::from_array(LANE_INDICES);
        let mut acc = Simd::<u16, LANES>::splat(u16::MAX);
        let mut singles: u128 = 0;
        for k in 0..PADDED / LANES {
            // SAFETY: k*LANES + LANES <= PADDED, so the load stays in bounds.
            let v = Simd::<u16, LANES>::from_slice(unsafe {
                self.cand.get_unchecked(k * LANES..k * LANES + LANES)
            });
            // count only the nine digit bits; a filled cell's digit bits are
            // don't-care, so the `is_filled` mask gates them out below.
            let pc = (v & digits).count_ones();
            let is_filled: Mask<i16, LANES> = (v & filled_bit).simd_ne(zero);
            // forced singles: exactly one candidate AND not filled.
            let single_lane = pc.simd_eq(one) & !is_filled;
            singles |= (single_lane.to_bitmask() as u128) << (k * LANES);
            let s = is_filled.select(penalty, pc);
            let idx = lane_idx + Simd::<u16, LANES>::splat((k * LANES) as u16);
            acc = acc.simd_min((s << seven) | idx);
        }
        let best = acc.reduce_min();
        let best_count = best >> 7;
        if best_count == 0 {
            return (usize::MAX, 0, 0, 0); // dead end
        }
        if best_count >= 100 {
            return (usize::MAX, 10, 0, 0); // solved
        }
        let i = (best & 0x7F) as usize;
        // SAFETY: i < 81 — it won a real (non-penalty) score.
        let mask = unsafe { *self.cand.get_unchecked(i) } & ALL_DIGITS;
        (i, best_count as u32, mask, singles)
    }

    /// Place every cell flagged in `singles`, re-reading each cell's current
    /// mask so a co-wave peer placement that emptied it is caught as a
    /// contradiction. Returns false on contradiction.
    ///
    /// Placing one single can only clear bits from another, so a cell that was a
    /// single either still holds its one digit or has dropped to zero (a genuine
    /// contradiction: its sole candidate was just used by a peer). No cell can
    /// gain candidates, so re-reading is sufficient — no need to recompute the
    /// wave.
    #[inline]
    fn place_singles(&mut self, mut singles: u128) -> bool {
        while singles != 0 {
            let c = singles.trailing_zeros() as usize;
            singles &= singles - 1;
            // SAFETY: c < 96 (bit index of a 96-bit mask) and only ever < 81
            // because padding lanes are FILLED and never flagged as singles.
            let m = unsafe { *self.cand.get_unchecked(c) } & ALL_DIGITS;
            if m == 0 {
                return false; // a co-wave peer consumed this cell's only digit
            }
            let d = m.trailing_zeros() as u8 + 1;
            self.place(c, d);
        }
        true
    }

    /// True if the grid has at least one completion.
    fn solve_first(&mut self) -> bool {
        loop {
            let (best_cell, best_count, best_mask, singles) = self.scan();
            if best_count == 0 {
                return false;
            }
            if best_cell == usize::MAX {
                return true;
            }
            // Drain the whole forced-single wave before considering a branch;
            // each wave is one scan instead of one-scan-per-single.
            if singles != 0 {
                if !self.place_singles(singles) {
                    return false;
                }
                continue;
            }
            // No singles: branch on the MRV cell (best_count >= 2 here).
            let mut m = best_mask;
            loop {
                let d = m.trailing_zeros() as u8 + 1;
                m &= m - 1;
                if m == 0 {
                    // Last alternative: place in self and re-scan (no clone).
                    self.place(best_cell, d);
                    break;
                }
                crate::counters::bump_clones();
                let mut child = self.clone();
                child.place(best_cell, d);
                if child.solve_first() {
                    return true;
                }
            }
        }
    }
}

/// `batch` existence probe: build the dense state once, clone+place per
/// alternate digit, then solve with batched single-placement.
pub struct Probe {
    base: FastSolver,
}

impl UniqProber for Probe {
    const NAME: &'static str = "batch";

    fn from_board(board: &Board) -> Self {
        Probe {
            base: FastSolver::from_board_candidates(board),
        }
    }

    #[inline]
    fn has_solution_with(&mut self, i: usize, d: u8) -> bool {
        crate::counters::bump_clones();
        let mut s = self.base.clone();
        s.place(i, d);
        s.solve_first()
    }
}
