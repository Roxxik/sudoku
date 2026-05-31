//! Variant `light-bv`: [`light`](super::light) made *fully* popcount-free.
//!
//! `light` already runs its single-cascade on a popcount-free scan, but at a
//! branch point it still calls a popcount MRV scan to pick the
//! minimum-candidate cell. On ARM (no 16-bit-lane popcount in wasm `simd128`)
//! that is the one expensive op left.
//!
//! The branch cell choice is a pure *efficiency* heuristic — it never affects
//! correctness or the strip trajectory (the fingerprint is fixed by the boolean
//! answers, not the search order). So we don't need the true minimum: we pick,
//! popcount-free, a **bi-value** cell (exactly two candidates — detected by
//! `vd & (vd-1)` being a non-zero power of two), falling back to the first empty
//! cell if none exists. A bi-value cell gives branching factor 2 (as good as
//! MRV's usual best on these near-full boards), so the search tree stays small
//! while every scan in the solve is now popcount-free.

use crate::grid::{ALL_DIGITS, Board, CELLS, PEERS};
use crate::solvers::UniqProber;
use std::simd::cmp::{SimdPartialEq};
use std::simd::{Mask, Simd};

const LANES: usize = 16;
const PADDED: usize = 96; // 6 * LANES, >= 81
const FILLED: u16 = 0x200;

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

    /// Branch-free placement, identical to the baseline.
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
    /// wave, using only the power-of-two test `vd & (vd - 1) == 0`.
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
            let empty: Mask<i16, LANES> = (v & filled_bit).simd_eq(zero);
            let is_zero = vd.simd_eq(zero) & empty;
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

    /// Popcount-free branch-cell pick: prefer a bi-value cell (exactly two
    /// candidates), else the first empty cell. Only called at a branch point
    /// (cascade drained, not solved), so every empty cell here has >= 2
    /// candidates. Returns `(cell, candidate_mask)`.
    #[inline]
    fn scan_branch(&self) -> (usize, u16) {
        crate::counters::bump_scans();
        let filled_bit = Simd::<u16, LANES>::splat(FILLED);
        let digits = Simd::<u16, LANES>::splat(ALL_DIGITS);
        let one = Simd::<u16, LANES>::splat(1);
        let zero = Simd::<u16, LANES>::splat(0);
        let mut bivalue: u128 = 0;
        let mut empties: u128 = 0;
        for k in 0..PADDED / LANES {
            // SAFETY: k*LANES + LANES <= PADDED.
            let v = Simd::<u16, LANES>::from_slice(unsafe {
                self.cand.get_unchecked(k * LANES..k * LANES + LANES)
            });
            let vd = v & digits;
            let empty: Mask<i16, LANES> = (v & filled_bit).simd_eq(zero);
            // bi-value: clearing the lowest set bit leaves a non-zero power of
            // two => exactly two bits were set. (Lanes with vd==0 give t==0,
            // excluded by `t != 0`; the `& empty` gate drops filled lanes.)
            let t = vd & (vd - one);
            let t_nonzero = t.simd_ne(zero);
            let t_pow2 = (t & (t - one)).simd_eq(zero);
            let biv = empty & t_nonzero & t_pow2;
            let base = k * LANES;
            bivalue |= (biv.to_bitmask() as u128) << base;
            empties |= (empty.to_bitmask() as u128) << base;
        }
        let cell = if bivalue != 0 {
            bivalue.trailing_zeros() as usize
        } else {
            empties.trailing_zeros() as usize
        };
        // SAFETY: cell < 81 — it is a set bit of `empties` (or `bivalue` ⊆
        // `empties`), and only real cells 0..81 are ever empty.
        let mask = unsafe { *self.cand.get_unchecked(cell) } & ALL_DIGITS;
        (cell, mask)
    }

    /// Place every cell flagged in `singles`, re-reading each cell's current
    /// mask so a co-wave peer placement that emptied it is caught as a
    /// contradiction. See [`light`](super::light) for why re-reading suffices.
    #[inline]
    fn place_singles(&mut self, mut singles: u128) -> bool {
        while singles != 0 {
            let c = singles.trailing_zeros() as usize;
            singles &= singles - 1;
            // SAFETY: c < 81 — only empty cells are ever flagged as singles.
            let m = unsafe { *self.cand.get_unchecked(c) } & ALL_DIGITS;
            if m == 0 {
                return false;
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
            if light.singles != 0 {
                if !self.place_singles(light.singles) {
                    return false;
                }
                continue;
            }
            if !light.any_empty {
                return true; // solved
            }
            // Branch on a bi-value (or first-empty) cell — popcount-free.
            let (best_cell, best_mask) = self.scan_branch();
            let mut m = best_mask;
            loop {
                let d = m.trailing_zeros() as u8 + 1;
                m &= m - 1;
                if m == 0 {
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

/// `light-bv` existence probe.
pub struct Probe {
    base: FastSolver,
}

impl UniqProber for Probe {
    const NAME: &'static str = "light-bv";

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
