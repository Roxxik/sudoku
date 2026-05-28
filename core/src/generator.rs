use crate::board::{Board, CELLS, iter_digits, popcount};
use crate::rng::Rng;
use crate::solver::solve;
use crate::techniques::TechniqueKind;
use crate::uniqueness;

pub struct GeneratedPuzzle {
    pub puzzle: Board,
    pub solution: Board,
    pub givens: usize,
}

pub fn random_full_grid(rng: &mut Rng) -> Board {
    let mut board = Board::empty();
    let ok = fill(&mut board, rng);
    debug_assert!(ok, "fill should always succeed on empty board");
    board
}

fn fill(board: &mut Board, rng: &mut Rng) -> bool {
    let mut best: Option<(usize, u16, u32)> = None;
    for i in 0..CELLS {
        if !board.is_empty(i) {
            continue;
        }
        let cs = board.candidates(i);
        let n = popcount(cs);
        if n == 0 {
            return false;
        }
        if best.map_or(true, |(_, _, bn)| n < bn) {
            best = Some((i, cs, n));
        }
    }
    let Some((cell, mask, _)) = best else {
        return true;
    };
    let mut digits: Vec<u8> = iter_digits(mask).collect();
    rng.shuffle(&mut digits);
    for d in digits {
        let backup = board.clone();
        board.place(cell, d);
        if fill(board, rng) {
            return true;
        }
        *board = backup;
    }
    false
}

pub struct FilteredResult {
    pub puzzle: GeneratedPuzzle,
    pub attempts: usize,
}

pub fn make_puzzle_forced(
    rng: &mut Rng,
    target: TechniqueKind,
    max_attempts: usize,
) -> Option<FilteredResult> {
    for attempt in 1..=max_attempts {
        let solution = random_full_grid(rng);
        let mut puzzle = solution.clone();
        let mut positions: Vec<usize> = (0..CELLS).collect();
        rng.shuffle(&mut positions);

        for i in positions {
            if puzzle.is_empty(i) {
                continue;
            }
            let mut candidate = puzzle.clone();
            candidate.clear(i);
            if uniqueness::count_solutions(&candidate, 2) != 1 {
                continue;
            }
            match solve_and_check_forced(&candidate, target) {
                Some(true) => {
                    let givens = (0..CELLS).filter(|&j| !candidate.is_empty(j)).count();
                    return Some(FilteredResult {
                        puzzle: GeneratedPuzzle {
                            puzzle: candidate,
                            solution,
                            givens,
                        },
                        attempts: attempt,
                    });
                }
                Some(false) => {
                    puzzle = candidate;
                }
                None => {} // not solvable, skip this strip
            }
        }
    }
    None
}

/// Returns Some(true) if the puzzle is technique-solvable AND requires
/// `target` (i.e., is not solvable using only the other techniques).
/// Some(false) if it is solvable without `target`. None if it isn't
/// technique-solvable at all.
fn solve_and_check_forced(board: &Board, target: TechniqueKind) -> Option<bool> {
    let mut b = board.clone();
    loop {
        if b.is_solved() {
            return Some(false);
        }
        match crate::solver::next_step_filtered(&b, |t| t != target) {
            Some(s) => crate::solver::apply_step(&mut b, &s),
            None => {
                // Filtered walk is stuck. The puzzle is solvable iff the
                // canonical solver can finish from here. Soundness of
                // deductions: every filtered step we already applied is a
                // true fact, so canonical-from-here agrees with canonical-
                // from-original on solvability.
                if crate::solver::solve_solvable_only(b) {
                    return Some(true);
                }
                return None;
            }
        }
    }
}

pub fn make_puzzle_needing(
    rng: &mut Rng,
    target: TechniqueKind,
    max_attempts: usize,
) -> Option<FilteredResult> {
    for attempt in 1..=max_attempts {
        let solution = random_full_grid(rng);
        let mut puzzle = solution.clone();
        let mut positions: Vec<usize> = (0..CELLS).collect();
        rng.shuffle(&mut positions);

        for i in positions {
            if puzzle.is_empty(i) {
                continue;
            }
            let mut candidate = puzzle.clone();
            candidate.clear(i);
            if uniqueness::count_solutions(&candidate, 2) != 1 {
                continue;
            }
            let result = solve(candidate.clone());
            if !result.solved {
                continue;
            }
            puzzle = candidate;
            if result.trace.iter().any(|s| s.technique == target) {
                let givens = (0..CELLS).filter(|&j| !puzzle.is_empty(j)).count();
                return Some(FilteredResult {
                    puzzle: GeneratedPuzzle {
                        puzzle,
                        solution,
                        givens,
                    },
                    attempts: attempt,
                });
            }
        }
    }
    None
}

pub fn make_puzzle(rng: &mut Rng, require_technique_solvable: bool) -> GeneratedPuzzle {
    let solution = random_full_grid(rng);
    let mut puzzle = solution.clone();

    let mut positions: Vec<usize> = (0..CELLS).collect();
    rng.shuffle(&mut positions);

    for i in positions {
        if puzzle.is_empty(i) {
            continue;
        }
        let mut candidate = puzzle.clone();
        candidate.clear(i);
        if uniqueness::count_solutions(&candidate, 2) != 1 {
            continue;
        }
        if require_technique_solvable {
            let result = solve(candidate.clone());
            if !result.solved {
                continue;
            }
        }
        puzzle = candidate;
    }

    let givens = (0..CELLS).filter(|&i| !puzzle.is_empty(i)).count();
    GeneratedPuzzle {
        puzzle,
        solution,
        givens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_grid_is_solved_and_valid() {
        let mut rng = Rng::from_seed(42);
        let grid = random_full_grid(&mut rng);
        assert!(grid.is_solved());
        assert_eq!(uniqueness::count_solutions(&grid, 2), 1);
    }

    #[test]
    fn full_grids_differ_with_different_seeds() {
        let a = random_full_grid(&mut Rng::from_seed(1)).to_line();
        let b = random_full_grid(&mut Rng::from_seed(2)).to_line();
        assert_ne!(a, b);
    }

    #[test]
    fn generated_puzzle_has_unique_solution() {
        let mut rng = Rng::from_seed(123);
        let out = make_puzzle(&mut rng, false);
        assert_eq!(uniqueness::count_solutions(&out.puzzle, 2), 1);
        assert!(out.givens < 81);
    }

    #[test]
    fn technique_solvable_puzzle_actually_solves() {
        let mut rng = Rng::from_seed(456);
        let out = make_puzzle(&mut rng, true);
        let result = solve(out.puzzle.clone());
        assert!(result.solved, "puzzle marked technique-solvable should solve");
        assert_eq!(result.board.to_line(), out.solution.to_line());
    }
}
