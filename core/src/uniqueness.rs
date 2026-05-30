use crate::board::{ALL_DIGITS, Board, CELLS, PEERS, iter_digits, popcount};
use std::simd::cmp::{SimdOrd, SimdPartialEq};
use std::simd::num::SimdUint;
use std::simd::{Mask, Simd};

/// Width of the lanes the dense MRV scan loads at once. 96 = 6 × 16 covers all
/// 81 cells with padding, and 16-wide u16 maps to one AVX512 / two AVX2 vectors
/// on the host.
const LANES: usize = 16;
const PADDED: usize = 96; // 6 * LANES, >= 81

/// Sentinel bit stored in a cell's candidate slot once the cell is filled (or
/// for the 81..96 padding lanes). It sits above the nine digit bits (0..8) so
/// that clearing a digit bit on a peer never disturbs it, and the dense scan
/// can recognise "this lane is not an empty cell" by testing this single bit.
const FILLED: u16 = 0x200;

/// A placement-only solver state, used by the existence/uniqueness search.
///
/// The full [`Board`] keeps a per-cell candidate mask (162 B) so technique
/// solvers can record arbitrary per-cell eliminations. A brute-force solver
/// never does that — it only *places* digits and reads candidates implied by
/// the placements.
///
/// Earlier this state kept 27 per-unit "used-digit" masks and recomputed
/// `candidates(i)` from three of them on every MRV visit, iterating only the
/// empty cells via a `u128` bitset. Profiling showed that hot loop was split
/// between the per-cell 3-load+OR+popcount and the *serial* `tzcnt` dependency
/// chain walking the bitset. Both go away here: we keep a *dense* per-cell
/// candidate array updated incrementally on `place` (clear one bit in each of
/// the cell's 20 peers) and find the MRV cell with a branch-free SIMD min over
/// all 96 lanes — no serial bit-walk, one load per cell, vectorised popcount.
#[derive(Clone)]
struct FastSolver {
    /// Per-cell candidate bitmask (bits 0..8). A filled cell (and the 81..96
    /// padding) instead carries the [`FILLED`] sentinel bit; its digit bits are
    /// don't-care. Padded to [`PADDED`] so the scan can load full vectors.
    cand: [u16; PADDED],
}

impl FastSolver {
    fn from_board(b: &Board) -> Self {
        let mut s = FastSolver {
            cand: [FILLED; PADDED],
        };
        // First mark every empty cell with the full digit set, then replay the
        // givens through `place` so peers get their bits cleared incrementally.
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

    #[inline]
    fn place(&mut self, i: usize, d: u8) {
        let clear = !(1u16 << (d as u16 - 1));
        // SAFETY: `i` is a valid cell index (0..81) and every `PEERS[i]` entry
        // is a cell index in 0..81; `cand` has 96 slots, so all accesses are in
        // bounds. Clearing a digit bit on a filled peer leaves its `FILLED`
        // sentinel (bit 9) untouched, so the sentinel stays recognisable.
        unsafe {
            *self.cand.get_unchecked_mut(i) = FILLED;
            for &p in PEERS.get_unchecked(i) {
                *self.cand.get_unchecked_mut(p) &= clear;
            }
        }
    }

    /// Scan all cells with SIMD and return `(best_cell, best_count, best_mask)`.
    /// `best_count` is the minimum candidate count over empty cells, with the
    /// conventions: `0` => some empty cell is unsatisfiable (dead end), and a
    /// returned `best_cell == usize::MAX` => no empty cells remain (solved).
    #[inline]
    fn scan(&self) -> (usize, u32, u16) {
        let digits = Simd::<u16, LANES>::splat(ALL_DIGITS);
        let filled_bit = Simd::<u16, LANES>::splat(FILLED);
        let penalty = Simd::<u16, LANES>::splat(100);
        let zero = Simd::<u16, LANES>::splat(0);
        let mut score = [Simd::<u16, LANES>::splat(0); PADDED / LANES];
        // Keep a running per-lane minimum across all chunks and reduce it once
        // at the end — one horizontal reduction instead of six.
        let mut acc = penalty;
        for (k, chunk) in score.iter_mut().enumerate() {
            // SAFETY: k*LANES + LANES <= PADDED, so the load stays in bounds.
            let v = Simd::<u16, LANES>::from_slice(unsafe {
                self.cand.get_unchecked(k * LANES..k * LANES + LANES)
            });
            // popcount of the nine digit bits => candidate count (0..9).
            let pc = (v & digits).count_ones();
            // Filled / padding lanes carry the sentinel: push them out of the
            // running with a large penalty so they never win the MRV min.
            let is_filled: Mask<i16, LANES> = (v & filled_bit).simd_ne(zero);
            let s = is_filled.select(penalty, pc);
            acc = acc.simd_min(s);
            *chunk = s;
        }
        let best_count = acc.reduce_min();
        if best_count == 0 {
            return (usize::MAX, 0, 0); // dead end (some empty cell has no candidate)
        }
        if best_count >= 100 {
            return (usize::MAX, 10, 0); // every cell filled — solved
        }
        // Locate the first lane whose score equals the minimum.
        let target = Simd::<u16, LANES>::splat(best_count);
        for (k, s) in score.iter().enumerate() {
            let bm = s.simd_eq(target).to_bitmask();
            if bm != 0 {
                let i = k * LANES + bm.trailing_zeros() as usize;
                // SAFETY: i < 81 here — it indexes a lane that matched a real
                // (non-penalty) score, which only empty cells produce.
                let mask = unsafe { *self.cand.get_unchecked(i) } & ALL_DIGITS;
                return (i, best_count as u32, mask);
            }
        }
        unreachable!()
    }

    /// True if the grid has at least one completion. Forced singles are applied
    /// in place (no clone); only a genuine branch clones the compact state.
    fn solve_first(&mut self) -> bool {
        loop {
            let (best_cell, best_count, best_mask) = self.scan();
            if best_count == 0 {
                return false; // some empty cell is unsatisfiable
            }
            if best_cell == usize::MAX {
                return true; // no empty cells left — solved
            }
            if best_count == 1 {
                let d = best_mask.trailing_zeros() as u8 + 1;
                self.place(best_cell, d);
                continue;
            }
            let mut m = best_mask;
            while m != 0 {
                let d = m.trailing_zeros() as u8 + 1;
                m &= m - 1;
                let mut child = self.clone();
                child.place(best_cell, d);
                if child.solve_first() {
                    return true;
                }
            }
            return false;
        }
    }
}

/// True if `board` has at least one completion. Equivalent to
/// `solve_unique(board).is_some()` but on the compact placement-only state, and
/// without materializing the solved grid — the form the strip-loop uniqueness
/// probes need.
pub fn has_solution(board: &Board) -> bool {
    FastSolver::from_board(board).solve_first()
}

pub fn count_solutions(board: &Board, limit: usize) -> usize {
    let mut count = 0;
    let mut work = board.clone();
    backtrack(&mut work, &mut count, limit);
    count
}

pub fn solve_unique(board: &Board) -> Option<Board> {
    let mut work = board.clone();
    if backtrack_first(&mut work) { Some(work) } else { None }
}

/// Collect up to `limit` complete solutions of `board`. Like
/// [`count_solutions`] but keeps the grids, so callers can diff two
/// completions against each other (e.g. to find the cells an alternate
/// solution diverges on). Order matches the backtracker's search order.
pub fn collect_solutions(board: &Board, limit: usize) -> Vec<Board> {
    let mut out = Vec::with_capacity(limit);
    let mut work = board.clone();
    backtrack_collect(&mut work, &mut out, limit);
    out
}

fn backtrack_collect(board: &mut Board, out: &mut Vec<Board>, limit: usize) {
    loop {
        if out.len() >= limit {
            return;
        }
        let mut best_cell: usize = CELLS;
        let mut best_mask: u16 = 0;
        let mut best_count: u32 = 10;
        for i in 0..CELLS {
            if !board.is_empty(i) {
                continue;
            }
            let cs = board.candidates(i);
            let n = popcount(cs);
            if n == 0 {
                return;
            }
            if n < best_count {
                best_count = n;
                best_cell = i;
                best_mask = cs;
                if n == 1 {
                    break;
                }
            }
        }
        if best_cell == CELLS {
            // No empty cells left — a complete solution.
            out.push(board.clone());
            return;
        }
        if best_count == 1 {
            let d = iter_digits(best_mask).next().unwrap();
            board.place(best_cell, d);
            continue;
        }
        for d in iter_digits(best_mask) {
            let backup = board.clone();
            board.place(best_cell, d);
            backtrack_collect(board, out, limit);
            *board = backup;
            if out.len() >= limit {
                return;
            }
        }
        return;
    }
}

/// Recursive backtracker with iterative unit-propagation.
///
/// Forced singles (cells with exactly one candidate) need no branching, so
/// we place them in a loop without cloning the board or recursing. That
/// eliminates two 243-byte memcpys (clone + restore) and a stack frame per
/// forced single, which dominate when puzzles are mostly singles-solvable.
///
/// Only when the best cell has 2+ candidates do we fall back to the classic
/// clone/place/recurse/restore branching loop.
fn backtrack(board: &mut Board, count: &mut usize, limit: usize) {
    loop {
        if *count >= limit {
            return;
        }
        let mut best_cell: usize = CELLS;
        let mut best_mask: u16 = 0;
        let mut best_count: u32 = 10;
        for i in 0..CELLS {
            if !board.is_empty(i) {
                continue;
            }
            let cs = board.candidates(i);
            let n = popcount(cs);
            if n == 0 {
                return;
            }
            if n < best_count {
                best_count = n;
                best_cell = i;
                best_mask = cs;
                if n == 1 {
                    break;
                }
            }
        }
        if best_cell == CELLS {
            // No empty cells left — a complete solution.
            *count += 1;
            return;
        }
        if best_count == 1 {
            // Forced placement. Apply in place and continue the loop;
            // no clone/recurse/restore needed.
            let d = iter_digits(best_mask).next().unwrap();
            board.place(best_cell, d);
            continue;
        }
        // Real branch point.
        for d in iter_digits(best_mask) {
            let backup = board.clone();
            board.place(best_cell, d);
            backtrack(board, count, limit);
            *board = backup;
            if *count >= limit {
                return;
            }
        }
        return;
    }
}

fn backtrack_first(board: &mut Board) -> bool {
    loop {
        let mut best_cell: usize = CELLS;
        let mut best_mask: u16 = 0;
        let mut best_count: u32 = 10;
        for i in 0..CELLS {
            if !board.is_empty(i) {
                continue;
            }
            let cs = board.candidates(i);
            let n = popcount(cs);
            if n == 0 {
                return false;
            }
            if n < best_count {
                best_count = n;
                best_cell = i;
                best_mask = cs;
                if n == 1 {
                    break;
                }
            }
        }
        if best_cell == CELLS {
            return true;
        }
        if best_count == 1 {
            // Forced — apply and keep going.
            let d = iter_digits(best_mask).next().unwrap();
            board.place(best_cell, d);
            continue;
        }
        for d in iter_digits(best_mask) {
            let backup = board.clone();
            board.place(best_cell, d);
            if backtrack_first(board) {
                return true;
            }
            *board = backup;
        }
        return false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_board_has_many_solutions() {
        let b = Board::empty();
        assert_eq!(count_solutions(&b, 2), 2);
    }

    #[test]
    fn unique_puzzle_counts_one() {
        let puzzle = "003020600900305001001806400008102900700000008006708200002609500800203009005010300";
        let b = Board::parse(puzzle).unwrap();
        assert_eq!(count_solutions(&b, 2), 1);
    }

    #[test]
    fn solve_unique_returns_solution() {
        let puzzle = "003020600900305001001806400008102900700000008006708200002609500800203009005010300";
        let b = Board::parse(puzzle).unwrap();
        let sol = solve_unique(&b).unwrap();
        assert!(sol.is_solved());
    }
}
