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

/// Lane-local indices `[0, 1, ..., LANES-1]`, added to a per-chunk base to form
/// each lane's global cell index for the packed single-pass MRV scan.
const LANE_INDICES: [u16; LANES] = {
    let mut a = [0u16; LANES];
    let mut i = 0;
    while i < LANES {
        a[i] = i as u16;
        i += 1;
    }
    a
};

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

    /// Build directly from a board's already-maintained candidate masks.
    ///
    /// A [`Board`] keeps an incremental per-cell candidate mask, so for an empty
    /// cell `b.candidates(i)` is already `ALL_DIGITS` minus its peers' filled
    /// digits — byte-for-byte the value [`from_board`](Self::from_board) would
    /// reconstruct by replaying every given through `place`. Copying it is O(81)
    /// instead of O(givens x 20).
    ///
    /// PRECONDITION: the board's candidate masks must equal the *naked*
    /// candidates (no technique eliminations applied) — true for the strip
    /// loop's freshly `recompute_candidates`'d boards. The debug assertion below
    /// cross-checks against the replay path.
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
        let filled_bit = Simd::<u16, LANES>::splat(FILLED);
        let penalty = Simd::<u16, LANES>::splat(100);
        let zero = Simd::<u16, LANES>::splat(0);
        let seven = Simd::<u16, LANES>::splat(7);
        let lane_idx = Simd::<u16, LANES>::from_array(LANE_INDICES);
        // Single-pass MRV: pack each lane as `(score << 7) | global_index`. The
        // global index is 0..95 (7 bits), the score 0..9 (or the 100 penalty),
        // so one `reduce_min` yields both the minimum candidate count *and* the
        // first lane achieving it (ties resolve to the lowest index) — no second
        // locate pass, no stored per-chunk score array.
        let mut acc = Simd::<u16, LANES>::splat(u16::MAX);
        for k in 0..PADDED / LANES {
            // SAFETY: k*LANES + LANES <= PADDED, so the load stays in bounds.
            let v = Simd::<u16, LANES>::from_slice(unsafe {
                self.cand.get_unchecked(k * LANES..k * LANES + LANES)
            });
            // popcount => candidate count (0..9) for empty cells (which carry
            // only digit bits 0..8). Filled / padding lanes carry the FILLED
            // sentinel and pop to 1 here, but are overwritten by the penalty
            // below, so masking off the sentinel first would be wasted work.
            let pc = v.count_ones();
            // Filled / padding lanes carry the sentinel: push them out of the
            // running with a large penalty so they never win the MRV min.
            let is_filled: Mask<i16, LANES> = (v & filled_bit).simd_ne(zero);
            let s = is_filled.select(penalty, pc);
            let idx = lane_idx + Simd::<u16, LANES>::splat((k * LANES) as u16);
            acc = acc.simd_min((s << seven) | idx);
        }
        let best = acc.reduce_min();
        let best_count = best >> 7;
        if best_count == 0 {
            return (usize::MAX, 0, 0); // dead end (some empty cell has no candidate)
        }
        if best_count >= 100 {
            return (usize::MAX, 10, 0); // every cell filled — solved
        }
        let i = (best & 0x7F) as usize;
        // SAFETY: i < 81 here — it indexes the lane that won a real (non-penalty)
        // score, which only empty cells produce.
        let mask = unsafe { *self.cand.get_unchecked(i) } & ALL_DIGITS;
        (i, best_count as u32, mask)
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

/// A reusable existence-probe built once from a board, then cheaply cloned and
/// extended with a single placement per probe.
///
/// The strip loop tests, for one cleared cell, several alternate digits against
/// the *same* base board. Building the dense [`FastSolver`] once and cloning the
/// compact 96-lane state per alternate avoids rebuilding a `Board` + dense array
/// for every probe (the old `has_solution(&candidate.clone().place(i,d))` path).
pub struct ExistenceProbe {
    base: FastSolver,
}

impl ExistenceProbe {
    pub fn from_board(board: &Board) -> Self {
        // Strip-loop boards carry naked candidate masks, so the compact state
        // can be copied straight from them (see `from_board_candidates`).
        ExistenceProbe {
            base: FastSolver::from_board_candidates(board),
        }
    }

    /// True iff placing digit `d` (1..=9) at the (empty) cell `i` of the board
    /// this probe was built from yields a grid with at least one completion.
    ///
    /// Equivalent to `has_solution(&b)` where `b` is the base board with `i` set
    /// to `d`: `place` only clears bits / sets the FILLED sentinel and those ops
    /// commute, so cloning the base and placing `i=d` reaches the same dense
    /// state as rebuilding from a board that already has `i=d` given.
    #[inline]
    pub fn has_solution_with(&self, i: usize, d: u8) -> bool {
        let mut s = self.base.clone();
        s.place(i, d);
        s.solve_first()
    }
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

    /// Cross-check the SIMD existence solver against the canonical backtracker
    /// over many pseudo-random partially-filled boards. `has_solution` must
    /// agree with `count_solutions(.., 1) > 0` on every single board — this is
    /// the safety net for the unsafe dense-array / SIMD scan code.
    #[test]
    fn has_solution_matches_count_solutions() {
        use crate::rng::Rng;
        let mut rng = Rng::from_seed(0xC0FFEE);
        for _ in 0..2000 {
            // Build a random partially-filled, internally-consistent board by
            // placing a random number of givens; skip illegal placements.
            let mut b = Board::empty();
            let givens = rng.range(40) + 1;
            for _ in 0..givens {
                let cell = rng.range(CELLS);
                if !b.is_empty(cell) {
                    continue;
                }
                let cs = b.candidates(cell);
                if cs == 0 {
                    continue;
                }
                let choices: Vec<u8> = iter_digits(cs).collect();
                let d = choices[rng.range(choices.len())];
                b.place(cell, d);
            }
            let fast = has_solution(&b);
            let canonical = count_solutions(&b, 1) > 0;
            assert_eq!(fast, canonical, "mismatch on board {}", b.to_string());
        }
    }

    /// Cross-check the strip-loop `ExistenceProbe` fast path (copy-from-board
    /// constructor + clone+place per alternate) against the canonical
    /// backtracker. Mirrors the strip loop's usage: clear one filled cell, then
    /// probe each alternate digit. Run in a debug build, this also exercises the
    /// `from_board_candidates` debug assertion.
    #[test]
    fn existence_probe_matches_count_solutions() {
        use crate::rng::Rng;
        let mut rng = Rng::from_seed(0x1234_5678);
        for _ in 0..60 {
            let mut b = Board::empty();
            let givens = rng.range(50) + 5;
            for _ in 0..givens {
                let cell = rng.range(CELLS);
                if !b.is_empty(cell) {
                    continue;
                }
                let cs = b.candidates(cell);
                if cs == 0 {
                    continue;
                }
                let choices: Vec<u8> = iter_digits(cs).collect();
                let d = choices[rng.range(choices.len())];
                b.place(cell, d);
            }
            // Clear one filled cell to recreate the strip-loop shape.
            let filled: Vec<usize> = (0..CELLS).filter(|&i| !b.is_empty(i)).collect();
            if filled.is_empty() {
                continue;
            }
            let i = filled[rng.range(filled.len())];
            b.clear(i);
            let probe = ExistenceProbe::from_board(&b);
            for d in iter_digits(b.candidates(i)) {
                let probe_says = probe.has_solution_with(i, d);
                let mut placed = b.clone();
                placed.place(i, d);
                let canonical = count_solutions(&placed, 1) > 0;
                assert_eq!(
                    probe_says, canonical,
                    "probe mismatch at cell {} digit {} on {}",
                    i, d, b.to_string()
                );
            }
        }
    }
}
