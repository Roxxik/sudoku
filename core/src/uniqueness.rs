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

fn backtrack(board: &mut Board, count: &mut usize, limit: usize) {
    if *count >= limit {
        return;
    }
    let mut best: Option<(usize, u16)> = None;
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
            best = Some((i, cs));
            if n == 1 {
                break;
            }
        }
    }
    match best {
        None => {
            *count += 1;
        }
        Some((cell, mask)) => {
            for d in iter_digits(mask) {
                let backup = board.clone();
                board.place(cell, d);
                backtrack(board, count, limit);
                *board = backup;
                if *count >= limit {
                    return;
                }
            }
        }
    }
}

fn backtrack_first(board: &mut Board) -> bool {
    let mut best: Option<(usize, u16)> = None;
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
            best = Some((i, cs));
            if n == 1 {
                break;
            }
        }
    }
    match best {
        None => true,
        Some((cell, mask)) => {
            for d in iter_digits(mask) {
                let backup = board.clone();
                board.place(cell, d);
                if backtrack_first(board) {
                    return true;
                }
                *board = backup;
            }
            false
        }
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
