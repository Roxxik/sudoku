//! Variant `bitboard`: a digit-transposed bitboard solver, tdoku-flavoured.
//!
//! Every variant so far keeps the baseline's *per-cell* candidate array
//! (`[u16; 96]`, bit `d` of lane `c` = "digit d fits cell c") and pays a
//! 20-peer scalar AND-sweep on every placement. With ~22k placements per strip
//! attempt that peer walk is the dominant native cost, and the counters confirm
//! the placement count is identical across all per-cell variants — they only
//! shuffle *scan* cost around.
//!
//! This variant transposes the state: nine 81-bit position boards, `board[d]`
//! bit `c` set iff digit `d+1` can still go at cell `c`, plus an `unsolved`
//! mask of cells not yet decided. The payoff is the placement:
//!
//! ```text
//!   place d at c:  unsolved   &= !bit(c)          // c is decided
//!                  board[d]   &= !PEER_MASK[c]    // no peer of c may be d
//! ```
//!
//! Two `u128` AND-NOTs, regardless of peer count. The usual third duty of a
//! placement — "remove the other eight digits from cell c" — is never done
//! eagerly: a decided cell's stale bits in the other boards are simply gated out
//! by `unsolved` everywhere they could matter (single/dead/branch detection all
//! `& unsolved`). Candidates of *unsolved* cells stay exactly correct, which is
//! all the search reads.
//!
//! Forced-single detection is the classic ones/twos sieve over the nine boards
//! (`twos |= ones & board[d]; ones |= board[d]`), so a cell with exactly one
//! candidate is `unsolved & ones & !twos` and a dead cell is `unsolved & !ones`
//! — no popcount, ever. Singles are placed a whole wave at a time, grouped by
//! digit (`singles & board[d]` is every cell forced to `d`), and the branch cell
//! is a bi-value pick (`twos & !threes`), also popcount-free. So like
//! `light-bv` it carries no `count_ones` on any hot path (the ARM win) while
//! making each placement an order of magnitude cheaper than the peer walk (the
//! native win).
//!
//! Correctness still rides on the shared oracle cross-check
//! (`tests/correctness.rs`): the search tree differs from the baseline, but the
//! boolean it returns must not, and the strip fingerprint pins that.

use crate::grid::{Board, CELLS, PEERS, iter_digits};
use crate::solvers::UniqProber;

/// `PEER_MASK[c]` has a bit set for each of cell `c`'s 20 peers (self
/// excluded), as an 81-bit value in a `u128`. Placement AND-NOTs a board with
/// it to forbid the just-placed digit on every peer in one op.
const PEER_MASK: [u128; CELLS] = build_peer_mask();

const fn build_peer_mask() -> [u128; CELLS] {
    let mut m = [0u128; CELLS];
    let mut i = 0;
    while i < CELLS {
        let mut k = 0;
        while k < 20 {
            m[i] |= 1u128 << PEERS[i][k];
            k += 1;
        }
        i += 1;
    }
    m
}

#[inline(always)]
const fn bit(c: usize) -> u128 {
    1u128 << c
}

#[derive(Clone)]
struct FastSolver {
    /// `board[d]` bit `c` set iff digit `d+1` is still placeable at cell `c`.
    /// Bits of *decided* cells are stale (never cleared) but always masked by
    /// `unsolved` before use.
    board: [u128; 9],
    /// Bit `c` set iff cell `c` is still empty.
    unsolved: u128,
}

/// ones/twos sieve result over the nine boards, ungated.
struct Sieve {
    /// Bit `c` set iff at least one board has bit `c`.
    ones: u128,
    /// Bit `c` set iff at least two boards have bit `c`.
    twos: u128,
}

impl FastSolver {
    fn from_board_candidates(b: &Board) -> Self {
        let mut s = FastSolver {
            board: [0u128; 9],
            unsolved: 0,
        };
        for c in 0..CELLS {
            if b.cell(c) == 0 {
                s.unsolved |= bit(c);
                for d in iter_digits(b.candidates(c)) {
                    s.board[(d - 1) as usize] |= bit(c);
                }
            }
        }
        s
    }

    /// Place digit `d` (1..=9) at cell `c`: decide the cell and forbid `d` on
    /// every peer. Two AND-NOTs, no peer loop.
    #[inline(always)]
    fn place(&mut self, c: usize, d: u8) {
        crate::counters::bump_placements();
        self.unsolved &= !bit(c);
        // SAFETY: d in 1..=9, so d-1 in 0..9 indexes `board`.
        unsafe {
            *self.board.get_unchecked_mut((d - 1) as usize) &= !PEER_MASK[c];
        }
    }

    /// ones/twos sieve: which cells have >=1 / >=2 candidates (ungated by
    /// `unsolved`). Popcount-free.
    #[inline(always)]
    fn sieve(&self) -> Sieve {
        let mut ones: u128 = 0;
        let mut twos: u128 = 0;
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let b = unsafe { *self.board.get_unchecked(d) };
            twos |= ones & b;
            ones |= b;
        }
        Sieve { ones, twos }
    }

    /// Place a whole wave of forced singles, grouped by digit. `singles` is a
    /// mask of cells each known to have exactly one candidate. Within a digit
    /// group, two singles that are peers are an immediate contradiction
    /// (returns false); contradictions created *across* the wave (a peer
    /// stripped to zero candidates) surface as a dead cell on the next sieve.
    #[inline]
    fn place_singles(&mut self, singles: u128) -> bool {
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let group = singles & unsafe { *self.board.get_unchecked(d) };
            if group == 0 {
                continue;
            }
            let mut g = group;
            let mut peers: u128 = 0;
            while g != 0 {
                let lo = g & g.wrapping_neg();
                let c = lo.trailing_zeros() as usize;
                g &= g - 1;
                // Another single of the same digit is this cell's peer: two
                // copies of `d` forced into one unit — unsatisfiable.
                if peers & lo != 0 {
                    return false;
                }
                peers |= PEER_MASK[c];
            }
            self.unsolved &= !group;
            self.board[d] &= !peers;
            crate::counters::bump_placements_by(group.count_ones() as u64);
        }
        true
    }

    /// Bi-value (or first-unsolved) branch cell and its candidate mask. Called
    /// only when the cascade has drained, so every unsolved cell has >=2
    /// candidates. Popcount-free: prefer a cell with exactly two candidates
    /// (`twos & !threes`), else the lowest unsolved cell. Returns
    /// `(cell, candidate_digit_bits)` where bit `d` set => digit `d+1` fits.
    #[inline]
    fn branch_cell(&self) -> (usize, u16) {
        let mut ones: u128 = 0;
        let mut twos: u128 = 0;
        let mut threes: u128 = 0;
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let b = unsafe { *self.board.get_unchecked(d) };
            threes |= twos & b;
            twos |= ones & b;
            ones |= b;
        }
        let bivalue = self.unsolved & twos & !threes;
        let pick = if bivalue != 0 { bivalue } else { self.unsolved };
        let c = pick.trailing_zeros() as usize;
        let cb = bit(c);
        let mut mask: u16 = 0;
        for d in 0..9 {
            if self.board[d] & cb != 0 {
                mask |= 1 << d;
            }
        }
        (c, mask)
    }

    /// True iff the grid has at least one completion.
    fn solve_first(&mut self) -> bool {
        loop {
            let Sieve { ones, twos } = self.sieve();
            if self.unsolved & !ones != 0 {
                return false; // an unsolved cell has no candidate
            }
            if self.unsolved == 0 {
                return true; // every cell decided
            }
            let singles = self.unsolved & ones & !twos;
            if singles != 0 {
                if !self.place_singles(singles) {
                    return false;
                }
                continue;
            }
            // Branch.
            let (cell, mask) = self.branch_cell();
            let mut m = mask;
            loop {
                let d = m.trailing_zeros() as u8 + 1;
                m &= m - 1;
                if m == 0 {
                    // Last alternative: place in self and re-loop (no clone).
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

/// `bitboard` existence probe: build the transposed state once, clone+place per
/// alternate digit, then solve with the popcount-free transposed cascade.
pub struct Probe {
    base: FastSolver,
}

impl UniqProber for Probe {
    const NAME: &'static str = "bitboard";

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
