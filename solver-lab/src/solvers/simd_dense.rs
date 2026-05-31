//! Variant `simd-dense`: the current `core::uniqueness` fast solver, copied
//! verbatim. This is the BASELINE every other variant must beat.
//!
//! Dense per-cell candidate array updated incrementally on `place`, with a
//! branch-free SIMD MRV scan over all 96 lanes. Forced singles are applied in
//! place; only genuine branches clone the compact state.
//!
//! Known cost (from profiling the generator): `scan()` re-scans all 96 lanes
//! after *every* placement, so an existence solve that cascades through k
//! forced singles does k full-board scans — quadratic in the cascade length.
//! That is the inefficiency later variants target.

use crate::grid::{ALL_DIGITS, Board, CELLS, PEERS};
use crate::solvers::UniqProber;
use std::simd::cmp::{SimdOrd, SimdPartialEq};
use std::simd::num::SimdUint;
use std::simd::{Mask, Select, Simd};

const LANES: usize = 16;
const PADDED: usize = 96; // 6 * LANES, >= 81

/// Sentinel bit marking a filled cell (or the 81..96 padding). Sits above the
/// nine digit bits so clearing a digit bit on a peer never disturbs it.
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
    /// Per-cell candidate bitmask (bits 0..8). A filled cell (and 81..96
    /// padding) instead carries the FILLED sentinel; its digit bits are
    /// don't-care.
    cand: [u16; PADDED],
}

impl FastSolver {
    fn from_board(b: &Board) -> Self {
        let mut s = FastSolver {
            cand: [FILLED; PADDED],
        };
        for i in 0..CELLS {
            if b.cell(i) == 0 {
                s.cand[i] = ALL_DIGITS;
            }
        }
        for i in 0..CELLS {
            let d = b.cell(i);
            if d != 0 {
                s.place(i, d);
            }
        }
        s
    }

    /// Build directly from a board's already-maintained naked candidate masks
    /// (O(81) vs O(givens x 20)). PRECONDITION: the masks equal the naked
    /// candidates; the debug assertion cross-checks against the replay path.
    fn from_board_candidates(b: &Board) -> Self {
        let mut s = FastSolver {
            cand: [FILLED; PADDED],
        };
        for i in 0..CELLS {
            if b.cell(i) == 0 {
                s.cand[i] = b.candidates(i);
            }
        }
        debug_assert_eq!(
            s.cand,
            FastSolver::from_board(b).cand,
            "from_board_candidates diverged from the replay path"
        );
        s
    }

    #[inline]
    fn place(&mut self, i: usize, d: u8) {
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

    /// Scan all cells with SIMD: `(best_cell, best_count, best_mask)`.
    /// `best_count == 0` => dead end; `best_cell == usize::MAX` => solved.
    #[inline]
    fn scan(&self) -> (usize, u32, u16) {
        let filled_bit = Simd::<u16, LANES>::splat(FILLED);
        let penalty = Simd::<u16, LANES>::splat(100);
        let zero = Simd::<u16, LANES>::splat(0);
        let seven = Simd::<u16, LANES>::splat(7);
        let lane_idx = Simd::<u16, LANES>::from_array(LANE_INDICES);
        let mut acc = Simd::<u16, LANES>::splat(u16::MAX);
        for k in 0..PADDED / LANES {
            // SAFETY: k*LANES + LANES <= PADDED, so the load stays in bounds.
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
        if best_count == 0 {
            return (usize::MAX, 0, 0); // dead end
        }
        if best_count >= 100 {
            return (usize::MAX, 10, 0); // solved
        }
        let i = (best & 0x7F) as usize;
        // SAFETY: i < 81 — it won a real (non-penalty) score, which only empty
        // cells produce.
        let mask = unsafe { *self.cand.get_unchecked(i) } & ALL_DIGITS;
        (i, best_count as u32, mask)
    }

    /// True if the grid has at least one completion.
    fn solve_first(&mut self) -> bool {
        loop {
            let (best_cell, best_count, best_mask) = self.scan();
            if best_count == 0 {
                return false;
            }
            if best_cell == usize::MAX {
                return true;
            }
            if best_count == 1 {
                let d = best_mask.trailing_zeros() as u8 + 1;
                self.place(best_cell, d);
                continue;
            }
            let mut m = best_mask;
            loop {
                let d = m.trailing_zeros() as u8 + 1;
                m &= m - 1;
                if m == 0 {
                    // Last alternative: place in self and re-scan instead of
                    // cloning + recursing.
                    self.place(best_cell, d);
                    break;
                }
                let mut child = self.clone();
                child.place(best_cell, d);
                if child.solve_first() {
                    return true;
                }
            }
            continue;
        }
    }
}

/// `simd-dense` existence probe: build the dense state once, clone+place per
/// alternate digit. Mirrors `core::uniqueness::ExistenceProbe`.
pub struct Probe {
    base: FastSolver,
}

impl UniqProber for Probe {
    const NAME: &'static str = "simd-dense";

    fn from_board(board: &Board) -> Self {
        Probe {
            base: FastSolver::from_board_candidates(board),
        }
    }

    #[inline]
    fn has_solution_with(&mut self, i: usize, d: u8) -> bool {
        let mut s = self.base.clone();
        s.place(i, d);
        s.solve_first()
    }
}
