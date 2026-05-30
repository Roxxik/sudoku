use crate::board::{ALL_DIGITS, Board, CELLS, iter_digits, popcount};

/// `[row, col, box]` unit index for each cell — lets the compact solver derive
/// `candidates(i)` from three small bitmasks without div/mod in the hot loop.
const CELL_UNITS: [[usize; 3]; 81] = build_cell_units();

const fn build_cell_units() -> [[usize; 3]; 81] {
    let mut t = [[0usize; 3]; 81];
    let mut i = 0;
    while i < 81 {
        let r = i / 9;
        let c = i % 9;
        t[i] = [r, c, (r / 3) * 3 + c / 3];
        i += 1;
    }
    t
}

/// A placement-only solver state, used by the existence/uniqueness search.
///
/// The full [`Board`] keeps a per-cell candidate mask (162 B) so technique
/// solvers can record arbitrary per-cell eliminations. A brute-force solver
/// never does that — it only *places* digits and reads candidates implied by
/// the placements — so its candidates are fully captured by per-unit
/// "used-digit" masks (27 × u16 = 54 B). That makes `place` three mask writes
/// instead of twenty peer updates, turns `candidates(i)` into a hot-cache
/// computation instead of a scattered array load, and shrinks the state we
/// clone at each branch. An `empties` bitset lets the MRV scan visit only the
/// empty cells instead of walking all 81.
#[derive(Clone)]
struct FastSolver {
    cells: [u8; 81],
    used_row: [u16; 9],
    used_col: [u16; 9],
    used_box: [u16; 9],
    empties: u128,
}

impl FastSolver {
    fn from_board(b: &Board) -> Self {
        let mut s = FastSolver {
            cells: [0; 81],
            used_row: [0; 9],
            used_col: [0; 9],
            used_box: [0; 9],
            empties: 0,
        };
        for i in 0..CELLS {
            let d = b.cell(i);
            if d == 0 {
                s.empties |= 1u128 << i;
            } else {
                s.place(i, d); // empties bit is already clear here — the clear is a no-op
            }
        }
        s
    }

    #[inline]
    fn candidates(&self, i: usize) -> u16 {
        // SAFETY: `i` is always a valid cell index (0..81 — it comes from the
        // `empties` bitset, whose bits are 0..80, or from a cell we just chose),
        // and every `CELL_UNITS` entry holds three unit indices in 0..9 by
        // construction. So all four accesses are in bounds. Eliding the checks
        // removes the ~18% of the hot scan the annotate attributed to `cmp/jae`
        // bounds-check branches.
        unsafe {
            let u = CELL_UNITS.get_unchecked(i);
            ALL_DIGITS
                & !(*self.used_row.get_unchecked(u[0])
                    | *self.used_col.get_unchecked(u[1])
                    | *self.used_box.get_unchecked(u[2]))
        }
    }

    #[inline]
    fn place(&mut self, i: usize, d: u8) {
        // SAFETY: see `candidates` — `i` in 0..81, `CELL_UNITS` entries in 0..9.
        unsafe {
            let u = CELL_UNITS.get_unchecked(i);
            let bit = 1u16 << (d as u16 - 1);
            *self.cells.get_unchecked_mut(i) = d;
            *self.used_row.get_unchecked_mut(u[0]) |= bit;
            *self.used_col.get_unchecked_mut(u[1]) |= bit;
            *self.used_box.get_unchecked_mut(u[2]) |= bit;
        }
        self.empties &= !(1u128 << i);
    }

    /// True if the grid has at least one completion. Forced singles are applied
    /// in place (no clone); only a genuine branch clones the compact state.
    fn solve_first(&mut self) -> bool {
        loop {
            let mut best_cell = usize::MAX;
            let mut best_mask = 0u16;
            let mut best_count = 10u32;
            let mut e = self.empties;
            while e != 0 {
                let i = e.trailing_zeros() as usize;
                e &= e - 1;
                let cand = self.candidates(i);
                let n = cand.count_ones();
                if n == 0 {
                    return false;
                }
                if n < best_count {
                    best_count = n;
                    best_cell = i;
                    best_mask = cand;
                    if n == 1 {
                        break;
                    }
                }
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
