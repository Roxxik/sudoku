//! Variant `bitboard-fb`: [`bitboard_simd`](super::bitboard_simd) with the
//! bi-value branch fold folded INTO the main sieve.
//!
//! `bitboard-simd`'s `branch_cell` runs a SECOND nine-board fold computing
//! `ones`/`twos`/`threes` from scratch, purely to pick a bi-value cell
//! (`twos & !threes`). The final `ones`/`twos` the main `sieve` already produced
//! cannot be reused to get `threes` — `threes` needs the per-digit *running
//! prefix* of `twos`, not its final value. So the only way to avoid a separate
//! branch fold is to compute `threes` in the main sieve itself, via the
//! symmetric three-level pass:
//!
//! ```text
//!   threes |= twos & b;   // already in >=2 boards, appears again => >=3
//!   twos   |= ones & b;   // already in >=1, appears again       => >=2
//!   ones   |= b;
//! ```
//!
//! Then `branch_cell` is a pure pick — no fold. The search tree is byte-for-byte
//! `bitboard-simd`'s (same bi-value heuristic, same clone count), so this is a
//! clean A/B on ONE question: is carrying `threes` through every sieve cheaper
//! than recomputing a full three-level fold only at branch points?
//!
//! The catch is that the sieve runs on *every* `solve_first` iteration —
//! including the many that place a singles wave and never branch — whereas the
//! old branch fold ran only at branch points. So this trades "extra work on all
//! iterations" for "no work at branches"; whether it wins depends on the
//! branch-to-iteration ratio. (First measurement: see the bench / the project
//! memo.)
//!
//! Everything else — representation, single-wave placement, the `any_alt_solves`
//! last-alternate-in-place trick — matches `bitboard-simd`, so the oracle
//! cross-check and strip fingerprint still pin correctness.

use crate::grid::{Board, CELLS, PEERS, iter_digits};
use crate::solvers::UniqProber;
use std::simd::Simd;
use std::simd::cmp::SimdPartialEq;

type B = Simd<u64, 2>;
const ZERO: B = Simd::from_array([0, 0]);

/// `PEER_MASK[c]` = cell `c`'s 20 peers (self excluded) as an 81-bit mask split
/// across two `u64` lanes. `BITS[c]` = the single-cell mask `1 << c`, same split.
const PEER_MASK: [[u64; 2]; CELLS] = build_peer_mask();
const BITS: [[u64; 2]; CELLS] = build_bits();

const fn build_peer_mask() -> [[u64; 2]; CELLS] {
    let mut m = [[0u64; 2]; CELLS];
    let mut i = 0;
    while i < CELLS {
        let mut k = 0;
        while k < 20 {
            let p = PEERS[i][k];
            m[i][p / 64] |= 1u64 << (p % 64);
            k += 1;
        }
        i += 1;
    }
    m
}

const fn build_bits() -> [[u64; 2]; CELLS] {
    let mut b = [[0u64; 2]; CELLS];
    let mut c = 0;
    while c < CELLS {
        b[c][c / 64] = 1u64 << (c % 64);
        c += 1;
    }
    b
}

#[inline(always)]
fn peer_mask(c: usize) -> B {
    Simd::from_array(PEER_MASK[c])
}

#[inline(always)]
fn bit(c: usize) -> B {
    Simd::from_array(BITS[c])
}

#[inline(always)]
fn nonzero(x: B) -> bool {
    x.simd_ne(ZERO).any()
}

/// Lowest set bit's index in the 128-bit value.
#[inline(always)]
fn trailing(x: B) -> u32 {
    let lo = x[0];
    if lo != 0 {
        lo.trailing_zeros()
    } else {
        64 + x[1].trailing_zeros()
    }
}

#[derive(Clone)]
struct FastSolver {
    /// `board[d]` bit `c` set iff digit `d+1` is still placeable at cell `c`.
    board: [B; 9],
    /// Bit `c` set iff cell `c` is still empty.
    unsolved: B,
}

/// Result of the symmetric three-level sieve: cells appearing in >=1, >=2, >=3
/// of the nine boards (ungated by `unsolved`).
struct Sieve {
    ones: B,
    twos: B,
    threes: B,
}

impl FastSolver {
    fn from_board_candidates(b: &Board) -> Self {
        let mut board = [[0u64; 2]; 9];
        let mut unsolved = [0u64; 2];
        for c in 0..CELLS {
            if b.cell(c) == 0 {
                unsolved[c / 64] |= 1u64 << (c % 64);
                for d in iter_digits(b.candidates(c)) {
                    board[(d - 1) as usize][c / 64] |= 1u64 << (c % 64);
                }
            }
        }
        FastSolver {
            board: board.map(Simd::from_array),
            unsolved: Simd::from_array(unsolved),
        }
    }

    /// Place digit `d` (1..=9) at cell `c`: decide the cell, forbid `d` on every
    /// peer. Two v128 AND-NOTs.
    #[inline(always)]
    fn place(&mut self, c: usize, d: u8) {
        crate::counters::bump_placements();
        self.unsolved &= !bit(c);
        // SAFETY: d in 1..=9, so d-1 in 0..9 indexes `board`.
        unsafe {
            *self.board.get_unchecked_mut((d - 1) as usize) &= !peer_mask(c);
        }
    }

    /// Symmetric three-level sieve over the nine boards (ungated). Popcount-free.
    /// Costs one extra `threes |= twos & b` (an AND + OR) per board versus
    /// `bitboard-simd`'s ones/twos-only sieve, in exchange for `branch_cell`
    /// doing no fold at all.
    #[inline(always)]
    fn sieve(&self) -> Sieve {
        let mut ones = ZERO;
        let mut twos = ZERO;
        let mut threes = ZERO;
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let b = unsafe { *self.board.get_unchecked(d) };
            threes |= twos & b;
            twos |= ones & b;
            ones |= b;
        }
        Sieve { ones, twos, threes }
    }

    /// Place a whole wave of forced singles, grouped by digit. See
    /// [`bitboard`](super::bitboard::Probe) for the correctness argument; here
    /// the two 64-bit halves of each digit group are walked independently.
    #[inline]
    fn place_singles(&mut self, singles: B) -> bool {
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let group = singles & unsafe { *self.board.get_unchecked(d) };
            if !nonzero(group) {
                continue;
            }
            let mut peers = ZERO;
            for lane in 0..2 {
                let mut g = group[lane];
                let base = lane * 64;
                while g != 0 {
                    let c = base + g.trailing_zeros() as usize;
                    g &= g - 1;
                    // Same-digit single sharing a unit with an earlier one:
                    // unsatisfiable.
                    if nonzero(peers & bit(c)) {
                        return false;
                    }
                    peers |= peer_mask(c);
                }
            }
            let new = unsafe { *self.board.get_unchecked(d) } & !peers;
            self.board[d] = new;
            self.unsolved &= !group;
            crate::counters::bump_placements_by((group[0].count_ones() + group[1].count_ones()) as u64);
        }
        true
    }

    /// Branch-cell mask read for a chosen cell `c` (its candidate digit bits).
    #[inline(always)]
    fn cell_mask(&self, c: usize) -> u16 {
        let cb = bit(c);
        let mut mask: u16 = 0;
        for d in 0..9 {
            if nonzero(self.board[d] & cb) {
                mask |= 1 << d;
            }
        }
        mask
    }

    /// True iff the grid has at least one completion.
    fn solve_first(&mut self) -> bool {
        loop {
            let Sieve { ones, twos, threes } = self.sieve();
            if nonzero(self.unsolved & !ones) {
                return false; // an unsolved cell has no candidate
            }
            if !nonzero(self.unsolved) {
                return true; // every cell decided
            }
            let singles = self.unsolved & ones & !twos;
            if nonzero(singles) {
                if !self.place_singles(singles) {
                    return false;
                }
                continue;
            }
            // Branch: bi-value cell (exactly two candidates) if any, else first
            // unsolved. `threes` came free out of the sieve — no second fold.
            let bivalue = self.unsolved & twos & !threes;
            let pick = if nonzero(bivalue) { bivalue } else { self.unsolved };
            let cell = trailing(pick) as usize;
            let mut m = self.cell_mask(cell);
            loop {
                let d = m.trailing_zeros() as u8 + 1;
                m &= m - 1;
                if m == 0 {
                    self.place(cell, d);
                    break;
                }
                crate::counters::bump_clones();
                let mut child = self.clone();
                child.place(cell, d);
                if child.solve_first() {
                    return true;
                }
            }
        }
    }
}

/// `bitboard-fb` existence probe.
pub struct Probe {
    base: FastSolver,
}

impl UniqProber for Probe {
    const NAME: &'static str = "bitboard-fb";

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

    /// Same last-alternate-in-place trick as `bitboard-simd`, so the only
    /// difference between the two variants is where `threes` is computed.
    fn any_alt_solves(&mut self, i: usize, alts: crate::grid::Mask) -> bool {
        let mut it = iter_digits(alts).peekable();
        while let Some(d) = it.next() {
            if it.peek().is_none() {
                self.base.place(i, d);
                return self.base.solve_first();
            }
            crate::counters::bump_clones();
            let mut s = self.base.clone();
            s.place(i, d);
            if s.solve_first() {
                return true;
            }
        }
        false
    }
}
