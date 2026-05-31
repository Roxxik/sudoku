//! Variant `light`: [`batch`](super::batch) with the popcount taken off the hot
//! path.
//!
//! Real ARM-mobile numbers (Nothing Phone 2a) showed `batch` leaving a lot on
//! the table versus [`propagate`](super::propagate): wasm `simd128` has no
//! 16-bit-lane popcount, so the `count_ones` over all 96 lanes that every
//! `batch`/`simd_dense` scan performs is *the* dominant cost on ARM. propagate
//! wins there by doing essentially no full-board scans.
//!
//! But detecting forced singles never needed a count — only "exactly one bit
//! set", which is the popcount-free power-of-two test `vd & (vd - 1) == 0`. So
//! the cascade runs on a **light scan**: per 96 lanes it derives, with no
//! popcount, the dead-end flag, the empty-cell flag, and the 96-bit mask of
//! forced singles. The expensive popcount MRV scan runs *only* when the cascade
//! drains and a branch cell is needed — rare, exactly as in propagate.
//!
//! Placement stays the baseline's branch-free SIMD AND-sweep (the reason
//! simd-dense/batch win on native), and singles are still placed a whole wave at
//! a time (the reason batch beats simd-dense). The branch order (SIMD MRV) is
//! unchanged, so the search tree and every answer match the baseline.

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

/// Result of the popcount-free cascade scan.
struct Light {
    /// Some empty cell has zero candidates — the grid is unsatisfiable.
    dead: bool,
    /// At least one cell is still empty (else the grid is solved).
    any_empty: bool,
    /// Bit `j` set iff empty cell `j` has exactly one candidate.
    singles: u128,
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

    /// Popcount-free SIMD pass: dead-end, any-empty, and the full forced-single
    /// wave, using only the power-of-two test `vd & (vd - 1) == 0`. This is the
    /// per-cascade-step scan; it never counts bits.
    #[inline]
    fn scan_light(&self) -> Light {
        crate::counters::bump_scans();
        let filled_bit = Simd::<u16, LANES>::splat(FILLED);
        let digits = Simd::<u16, LANES>::splat(ALL_DIGITS);
        let one = Simd::<u16, LANES>::splat(1);
        let zero = Simd::<u16, LANES>::splat(0);
        let mut singles: u128 = 0;
        let mut zeros: u128 = 0;
        let mut empties: u128 = 0;
        for k in 0..PADDED / LANES {
            // SAFETY: k*LANES + LANES <= PADDED, so the load stays in bounds.
            let v = Simd::<u16, LANES>::from_slice(unsafe {
                self.cand.get_unchecked(k * LANES..k * LANES + LANES)
            });
            let vd = v & digits;
            // Padding lanes (81..96) carry FILLED, so `empty` excludes them.
            let empty: Mask<i16, LANES> = (v & filled_bit).simd_eq(zero);
            let is_zero = vd.simd_eq(zero) & empty;
            // power-of-two (incl. zero); gated to a real single by `& !is_zero`.
            let is_pow2 = (vd & (vd - one)).simd_eq(zero);
            let single = is_pow2 & empty & !is_zero;
            let base = k * LANES;
            singles |= (single.to_bitmask() as u128) << base;
            zeros |= (is_zero.to_bitmask() as u128) << base;
            empties |= (empty.to_bitmask() as u128) << base;
        }
        Light {
            dead: zeros != 0,
            any_empty: empties != 0,
            singles,
        }
    }

    /// Popcount MRV scan, identical to the baseline's. Only ever called at a
    /// branch point (cascade drained, grid not solved), so the winning count is
    /// always >= 2. Returns `(cell, candidate_mask)`.
    #[inline]
    fn scan_mrv(&self) -> (usize, u16) {
        crate::counters::bump_scans();
        let filled_bit = Simd::<u16, LANES>::splat(FILLED);
        let digits = Simd::<u16, LANES>::splat(ALL_DIGITS);
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
            let pc = (v & digits).count_ones();
            let is_filled: Mask<i16, LANES> = (v & filled_bit).simd_ne(zero);
            let s = is_filled.select(penalty, pc);
            let idx = lane_idx + Simd::<u16, LANES>::splat((k * LANES) as u16);
            acc = acc.simd_min((s << seven) | idx);
        }
        let best = acc.reduce_min();
        let i = (best & 0x7F) as usize;
        // SAFETY: i < 81 — it won a real (non-penalty) score.
        let mask = unsafe { *self.cand.get_unchecked(i) } & ALL_DIGITS;
        (i, mask)
    }

    /// Place every cell flagged in `singles`, re-reading each cell's current
    /// mask so a co-wave peer placement that emptied it is caught as a
    /// contradiction. Returns false on contradiction. See [`batch`](super::batch)
    /// for why re-reading (rather than recomputing the wave) is sufficient.
    #[inline]
    fn place_singles(&mut self, mut singles: u128) -> bool {
        while singles != 0 {
            let c = singles.trailing_zeros() as usize;
            singles &= singles - 1;
            // SAFETY: c < 81 — only empty cells are ever flagged as singles.
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
            let light = self.scan_light();
            if light.dead {
                return false;
            }
            // Drain the whole forced-single wave (popcount-free) before a branch.
            if light.singles != 0 {
                if !self.place_singles(light.singles) {
                    return false;
                }
                continue;
            }
            if !light.any_empty {
                return true; // solved
            }
            // Branch on the MRV cell — the only place that pays for popcount.
            let (best_cell, best_mask) = self.scan_mrv();
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

/// `light` existence probe: build the dense state once, clone+place per
/// alternate digit, then solve with popcount-free cascades.
pub struct Probe {
    base: FastSolver,
}

impl UniqProber for Probe {
    const NAME: &'static str = "light";

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
