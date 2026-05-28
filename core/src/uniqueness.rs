use crate::board::{Board, CELLS, iter_digits, popcount};

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
