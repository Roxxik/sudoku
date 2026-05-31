//! Variant `bitboard-simd`: [`bitboard`](super::bitboard) with the 81-bit
//! boards held in a 128-bit SIMD lane pair instead of a scalar `u128`.
//!
//! `bitboard` already wins on every x86 host by transposing the state to nine
//! position boards and making a placement two `u128` AND-NOTs. But a `u128` has
//! no hardware home on wasm: every AND/OR lowers to *two* i64 ops. The whole hot
//! path here is bitwise (the ones/twos sieve, the AND-NOT placement, the
//! bi-value branch), so on `simd128` targets — including the ARM phone, the real
//! mobile target — folding each board into one `v128` register halves those
//! ops. On native the same `Simd<u64, 2>` maps to one SSE2 register.
//!
//! Everything else is identical to [`bitboard`]: same transposed representation,
//! same lazy `unsolved`-gated decided cells, same popcount-free cascade and
//! branch, so the search tree (and the boolean it returns) matches. The only
//! difference is the lane type and that bit-walking iterates the two 64-bit
//! halves of each board explicitly (a `u128` clear-lowest-bit would carry across
//! the halves; two independent `u64` walks avoid that).

use crate::grid::{Board, CELLS, PEERS, iter_digits};
use crate::solvers::UniqProber;
use std::simd::Simd;
use std::simd::cmp::SimdPartialEq;

type B = Simd<u64, 2>;
const ZERO: B = Simd::from_array([0, 0]);

/// `PEER_MASK[c]` = cell `c`'s 20 peers (self excluded) as an 81-bit mask split
/// across two `u64` lanes (lane 0 = cells 0..64, lane 1 = cells 64..81).
const PEER_MASK: [[u64; 2]; CELLS] = build_peer_mask();
/// `BITS[c]` = the single-cell mask `1 << c`, same two-lane split.
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

/// Lowest set bit's index in the 128-bit value, or 128 if empty.
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

struct Sieve {
    ones: B,
    twos: B,
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

    /// ones/twos sieve over the nine boards (ungated). Popcount-free.
    #[inline(always)]
    fn sieve(&self) -> Sieve {
        let mut ones = ZERO;
        let mut twos = ZERO;
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let b = unsafe { *self.board.get_unchecked(d) };
            twos |= ones & b;
            ones |= b;
        }
        Sieve { ones, twos }
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
                    let pm = peer_mask(c);
                    // Same-digit single sharing a unit with an earlier one:
                    // unsatisfiable.
                    if nonzero(peers & bit(c)) {
                        return false;
                    }
                    peers |= pm;
                }
            }
            self.unsolved &= !group;
            self.board[d] &= !peers;
            crate::counters::bump_placements_by((group[0].count_ones() + group[1].count_ones()) as u64);
        }
        true
    }

    /// Bi-value (or first-unsolved) branch cell and its candidate-digit mask.
    /// Popcount-free. Only called with the cascade drained (every unsolved cell
    /// has >=2 candidates).
    #[inline]
    fn branch_cell(&self) -> (usize, u16) {
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
        let bivalue = self.unsolved & twos & !threes;
        let pick = if nonzero(bivalue) { bivalue } else { self.unsolved };
        let c = trailing(pick) as usize;
        let cb = bit(c);
        let mut mask: u16 = 0;
        for d in 0..9 {
            if nonzero(self.board[d] & cb) {
                mask |= 1 << d;
            }
        }
        (c, mask)
    }

    /// True iff the grid has at least one completion.
    fn solve_first(&mut self) -> bool {
        loop {
            let Sieve { ones, twos } = self.sieve();
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
            let (cell, mask) = self.branch_cell();
            let mut m = mask;
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

/// `bitboard-simd` existence probe.
pub struct Probe {
    base: FastSolver,
}

impl UniqProber for Probe {
    const NAME: &'static str = "bitboard-simd";

    fn from_board(board: &Board) -> Self {
        crate::counters::bump_build();
        Probe {
            base: FastSolver::from_board_candidates(board),
        }
    }

    #[inline]
    fn has_solution_with(&mut self, i: usize, d: u8) -> bool {
        crate::counters::bump_clones();
        crate::counters::bump_entry_clone();
        let mut s = self.base.clone();
        s.place(i, d);
        s.solve_first()
    }

    /// Override: the probe is dropped after this returns, so the *last* alternate
    /// can be solved on `base` itself — no clone. Earlier alternates still clone
    /// (they may be followed by another query that needs `base` intact). Removes
    /// ~5% of clones with no added per-placement cost — provably never slower, but
    /// measured neutral (clones are not the bottleneck; the in-place trail
    /// experiment showed removing *all* clones is actually slower). Kept because
    /// `any_alt_solves` is also the cleaner call-site shape. Answers identical to
    /// the default loop.
    fn any_alt_solves(&mut self, i: usize, alts: crate::grid::Mask) -> bool {
        let mut it = iter_digits(alts).peekable();
        while let Some(d) = it.next() {
            if it.peek().is_none() {
                // Last alternate: consume `base` in place, no clone.
                self.base.place(i, d);
                return self.base.solve_first();
            }
            crate::counters::bump_clones();
            crate::counters::bump_entry_clone();
            let mut s = self.base.clone();
            s.place(i, d);
            if s.solve_first() {
                return true;
            }
        }
        false
    }
}
