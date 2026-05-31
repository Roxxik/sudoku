//! Canonical solution counter, copied from `core::uniqueness::count_solutions`.
//! Deliberately simple (scalar MRV backtracker on the full `Board`) — it is the
//! trusted oracle every fast-solver variant is cross-checked against, so it
//! values obvious-correctness over speed.

use crate::grid::{Board, CELLS, iter_digits, popcount};

/// Count completions of `board`, stopping once `limit` are found.
pub fn count_solutions(board: &Board, limit: usize) -> usize {
    let mut count = 0;
    let mut work = board.clone();
    backtrack(&mut work, &mut count, limit);
    count
}

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
            *count += 1;
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
            backtrack(board, count, limit);
            *board = backup;
            if *count >= limit {
                return;
            }
        }
        return;
    }
}
