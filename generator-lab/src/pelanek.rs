//! Pelánek (2014) difficulty metrics: the **SiSuS** human-solver model and the
//! two difficulty metrics it produces — **Refutation sum** (step complexity) and
//! **Dependency** (frontier width). A faithful, parameter-light implementation of
//! `docs/pelanek-2014-sisus-refutation-dependency.md`.
//!
//! Unlike the per-node relative [`grade`](crate::grade) grader (which bands one
//! campaign node's yield against itself off a single deterministic solve), these
//! are **absolute, portable** metrics correlated with human solving time, built
//! from a *randomized* model of how a person solves:
//!
//! - The model only ever uses the two **simple techniques** (naked single, hidden
//!   single) at equal weight. There are no per-technique numeric parameters.
//! - When the simple techniques stall, the model scores every empty cell by a
//!   **refutation rollout** — tentatively place each *wrong* candidate and count
//!   the random single-steps to a contradiction — and fills the cheapest cell
//!   with its known-solution digit, recording that cost as the step's difficulty.
//!
//! The whole pipeline is built on the scalar [`Board`] (placements + per-cell
//! candidate marks), with the unique solution obtained via the
//! [`first_completion`](crate::probe::search::first_completion) prober so an
//! *arbitrary* puzzle (e.g. one loaded from a human-difficulty dataset) can be
//! graded, not just a freshly generated one.

use crate::probe::search::first_completion;
use crate::repr::banded::{Bands, RowMajor};
use crate::repr::{
    Board, CELLS, CellIdx, Digit, DigitGrid, GridMask, Marks, Solution, SolverState, UNITS,
};
use crate::rng::Rng;
use crate::scan::Bivalue;

/// The packing the solution prober runs on — the lab's standard banded board.
type SolveM = Bands<RowMajor>;

/// The unique completion of `puzzle` as its known answer `v*`, or `None` if the
/// puzzle has no completion. For a well-posed (uniquely solvable) puzzle the first
/// completion the search reaches *is* the only one — the model needs `v*` to fill
/// the cell no simple technique resolves.
///
/// At a solved grid each empty cell lands in exactly one of the prober's per-digit
/// boards (the stale-free property), so the answer reads straight out of the
/// completed [`SolverState`] (the same reconstruction the UA-witness diff uses).
pub fn solution_of(puzzle: &DigitGrid) -> Option<Solution> {
    let comp = first_completion::<SolveM, Bivalue>(SolverState::<SolveM>::from_digits(puzzle))?;
    let mut grid = DigitGrid::EMPTY;
    for c in 0..CELLS {
        if let Some(d) = puzzle.get(c) {
            grid.set(c, d); // a given: in no candidate board, read from the puzzle
            continue;
        }
        let mut placed = None;
        for di in 0..9 {
            if (comp.candidates().each()[di] & SolveM::cell(c)).any() {
                placed = Some(Digit::from_index(di));
                break;
            }
        }
        grid.set(c, placed.expect("a solved completion holds a digit for every cell"));
    }
    Some(Solution(grid))
}

/// Whether `board` is inconsistent — the two failure signatures the refutation
/// rollout watches for (§0 of the spec):
/// - **(a)** an empty cell whose candidate set is empty (nowhere to go), or
/// - **(b)** a unit holding a digit that is neither placed in it nor a candidate
///   of any of its empty cells (a digit with no home).
///
/// Both are needed: an all-excluded empty cell (a) need not leave any unit
/// short a home, and a homeless digit (b) need not empty any single cell.
fn is_inconsistent(board: &Board) -> bool {
    for c in 0..CELLS {
        if board.cell(c).is_none() && board.marks(c).is_empty() {
            return true; // (a)
        }
    }
    for unit in &UNITS {
        // The digits still reachable in this unit: union of its placed digits and
        // the candidate sets of its empty cells. A well-formed unit reaches all 9.
        let mut available = 0u16;
        for &c in unit {
            available |= match board.cell(c) {
                Some(d) => 1u16 << d.index(),
                None => board.marks(c).bits(),
            };
        }
        if available != 0x1FF {
            return true; // (b)
        }
    }
    false
}

/// Collect the **simple-fillable set** of `board` into `out`: every cell currently
/// forced by a naked or hidden single, as `(cell, forced digit)`, **deduplicated
/// by cell**. `out` is cleared first and reused across calls to avoid allocation.
///
/// The count `out.len()` is the Dependency metric's per-step `possibilities`; a
/// uniformly random element is one SiSuS move. Dedup by cell is what makes the two
/// techniques agree on a single move per cell: a naked single's lone candidate is
/// the only digit any hidden single could target it with, so a cell never appears
/// with two different digits (which would be a contradiction `is_inconsistent`
/// catches, and would otherwise risk placing an impossible digit).
fn simple_moves(board: &Board, out: &mut Vec<(CellIdx, Digit)>) {
    out.clear();
    let mut seen = [false; CELLS];
    // Naked singles: an empty cell with exactly one candidate.
    for c in 0..CELLS {
        if board.cell(c).is_some() {
            continue;
        }
        let m = board.marks(c);
        if m.len() == 1 {
            out.push((c, m.iter().next().expect("exactly one candidate")));
            seen[c] = true;
        }
    }
    // Hidden singles: a (unit, digit) whose digit has exactly one candidate cell.
    for unit in &UNITS {
        for di in 0..9 {
            let d = Digit::from_index(di);
            let mut only = None;
            let mut count = 0u8;
            for &c in unit {
                if board.cell(c).is_none() && board.marks(c).contains(d) {
                    only = Some(c);
                    count += 1;
                    if count > 1 {
                        break;
                    }
                }
            }
            if count == 1 {
                let c = only.expect("count == 1");
                if !seen[c] {
                    out.push((c, d));
                    seen[c] = true;
                }
            }
        }
    }
}

/// The fewest-candidate empty cell — the defensive fallback for the (impossible on
/// a well-posed 9×9) all-infinite stuck state, so the model still makes progress.
fn fewest_candidate_cell(board: &Board) -> CellIdx {
    (0..CELLS)
        .filter(|&c| board.cell(c).is_none())
        .min_by_key(|&c| board.marks(c).len())
        .expect("an incomplete board has an empty cell")
}

/// A single randomized refutation rollout (§3.1): place wrong candidate `v` in
/// cell `c` on a scratch copy of `board`, then apply uniformly-random simple steps
/// until a contradiction surfaces. Returns the step count to the contradiction, or
/// `None` (= infinity) if propagation stalls with no simple move and no
/// contradiction — i.e. `v` is not refutable by singles alone.
///
/// The contradiction is checked *before* the first step, so an immediately-broken
/// placement scores 0. Because `v` is wrong, no valid completion has `c = v`, so a
/// rollout can never fill the grid to a valid solution: it always terminates in a
/// contradiction (finite) or an early stall (infinite). `scratch` is the reused
/// move buffer.
fn ref_v(
    board: &Board,
    c: CellIdx,
    v: Digit,
    rng: &mut Rng,
    scratch: &mut Vec<(CellIdx, Digit)>,
) -> Option<u32> {
    let mut s = board.clone();
    s.place(c, v);
    let mut steps = 0u32;
    loop {
        if is_inconsistent(&s) {
            return Some(steps);
        }
        simple_moves(&s, scratch);
        if scratch.is_empty() {
            return None; // stalled without contradiction -> not refutable by singles
        }
        let (cell, d) = scratch[rng.range(scratch.len())];
        s.place(cell, d);
        steps += 1;
    }
}

/// The ideal refutation score of empty cell `c` (§3): the sum over `c`'s **wrong**
/// candidates (every candidate but the solution digit `v*`) of [`ref_v`]. `None`
/// (= infinity) if *any* wrong candidate is not refutable by singles — a cell is
/// cheap only when every wrong candidate dies quickly under pure propagation.
fn refutation_score(
    board: &Board,
    c: CellIdx,
    solution: &Solution,
    rng: &mut Rng,
    scratch: &mut Vec<(CellIdx, Digit)>,
) -> Option<u64> {
    let v_star = solution.get(c).expect("solution is complete");
    let mut total = 0u64;
    for v in board.marks(c).iter() {
        if v == v_star {
            continue;
        }
        total += ref_v(board, c, v, rng, scratch)? as u64; // `?` short-circuits to infinity
    }
    Some(total)
}

/// One SiSuS run's instrumentation — the raw material both metrics read.
pub struct RunLog {
    /// One entry per **stuck** step: the refutation score recorded when no simple
    /// technique applied and the cheapest cell was filled from the solution.
    pub recorded_scores: Vec<u64>,
    /// One entry per **step** (forced or stuck): the simple-fillable count
    /// (`possibilities`) at that step. Zero on a stuck step.
    pub possibilities: Vec<u32>,
}

/// Run the SiSuS model to completion on `puzzle` (with known `solution`), logging
/// the refutation score of each stuck step and the per-step possibility count
/// (§2.1). Exactly one cell is filled per step, so the number of steps equals the
/// number of empty cells — identical across runs of the same puzzle, which is what
/// lets the Dependency curve be averaged per step index.
pub fn sisus_run(puzzle: &DigitGrid, solution: &Solution, rng: &mut Rng) -> RunLog {
    let mut board = Board::from_digits(puzzle);
    let mut moves: Vec<(CellIdx, Digit)> = Vec::with_capacity(CELLS);
    let mut scratch: Vec<(CellIdx, Digit)> = Vec::with_capacity(CELLS);
    let mut log = RunLog { recorded_scores: Vec::new(), possibilities: Vec::new() };

    while !board.is_complete() {
        simple_moves(&board, &mut moves);
        if !moves.is_empty() {
            // A simple step: record the frontier width, fill a random forced cell.
            log.possibilities.push(moves.len() as u32);
            let (c, d) = moves[rng.range(moves.len())];
            board.place(c, d);
            continue;
        }
        // Stuck: score every empty cell, fill the cheapest with its solution digit.
        log.possibilities.push(0);
        let mut best_cell = None;
        let mut best_score = u64::MAX;
        let mut ties = 0u32; // reservoir sampling for a uniform argmin tie-break
        for c in 0..CELLS {
            if board.cell(c).is_some() {
                continue;
            }
            let Some(score) = refutation_score(&board, c, solution, rng, &mut scratch) else {
                continue; // infinite: never the minimum
            };
            if score < best_score {
                best_score = score;
                best_cell = Some(c);
                ties = 1;
            } else if score == best_score {
                ties += 1;
                if rng.range(ties as usize) == 0 {
                    best_cell = Some(c);
                }
            }
        }
        let (c, recorded) = match best_cell {
            Some(c) => (c, best_score),
            None => {
                // The paper: a stuck 9×9 always has a finite-score cell. Guard the
                // claim under test; fill the fewest-candidate cell if it ever fails.
                debug_assert!(false, "all empty cells have infinite refutation score");
                (fewest_candidate_cell(&board), 0)
            }
        };
        board.place(c, solution.get(c).expect("solution is complete"));
        log.recorded_scores.push(recorded);
    }
    log
}

/// Tunable knobs — all default to the values the paper pins.
pub struct Opts {
    /// Runs to average the Refutation sum over (paper: 30).
    pub refutation_runs: usize,
    /// Runs to average the Dependency per-step curve over (paper: "several runs").
    pub dependency_runs: usize,
    /// Steps to average Dependency over (paper: k ≈ 25; the optimum band is 20..30
    /// and the metric is insensitive inside it).
    pub dependency_k: usize,
}

impl Default for Opts {
    fn default() -> Self {
        Self { refutation_runs: 30, dependency_runs: 30, dependency_k: 25 }
    }
}

/// The two Pelánek metrics for one puzzle.
pub struct Pelanek {
    /// **Refutation sum**: mean over runs of the per-run sum of recorded
    /// refutation scores. Higher = harder; ≈0 for a singles-only puzzle (the
    /// fallback never fires), which is exactly why it cannot discriminate easy
    /// puzzles and Dependency is its needed complement.
    pub refutation_sum: f64,
    /// **Dependency**: mean simple-fillable count over the first `k` steps,
    /// averaged per step index over runs. **Inverse to difficulty** — *larger =
    /// easier* (fewer simultaneous options = a narrower, more sequential forced
    /// chain = harder). Feed it a negative coefficient in any difficulty model.
    pub dependency: f64,
}

/// Compute both metrics for `puzzle` given its known `solution`, seeding the model
/// RNG from `seed` (so the result is reproducible). Shares one set of runs between
/// the two metrics (the runs are independent of which metric reads them).
pub fn pelanek_metrics(puzzle: &DigitGrid, solution: &Solution, seed: u64, opts: &Opts) -> Pelanek {
    let runs = opts.refutation_runs.max(opts.dependency_runs).max(1);
    let mut rng = Rng::from_seed(seed);
    let logs: Vec<RunLog> = (0..runs).map(|_| sisus_run(puzzle, solution, &mut rng)).collect();

    // Refutation sum: mean per-run total of recorded scores.
    let ref_runs = opts.refutation_runs.clamp(1, runs);
    let refutation_sum = logs[..ref_runs]
        .iter()
        .map(|l| l.recorded_scores.iter().map(|&s| s as f64).sum::<f64>())
        .sum::<f64>()
        / ref_runs as f64;

    // Dependency: average possibilities(i) over runs for each i, then mean over the
    // first k steps. Every run has the same step count, so the curve is well-defined.
    let dep_runs = opts.dependency_runs.clamp(1, runs);
    let n_steps = logs[0].possibilities.len();
    let k = opts.dependency_k.min(n_steps);
    let dependency = if k == 0 {
        0.0
    } else {
        let mut acc = 0.0;
        for i in 0..k {
            let mean_i = logs[..dep_runs].iter().map(|l| l.possibilities[i] as f64).sum::<f64>()
                / dep_runs as f64;
            acc += mean_i;
        }
        acc / k as f64
    };

    Pelanek { refutation_sum, dependency }
}

/// Grade a `puzzle` end to end: solve it for `v*`, then compute both metrics.
/// `None` if the puzzle has no completion.
pub fn grade_puzzle(puzzle: &DigitGrid, seed: u64, opts: &Opts) -> Option<Pelanek> {
    let solution = solution_of(puzzle)?;
    Some(pelanek_metrics(puzzle, &solution, seed, opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A classic puzzle and its unique solution (shared with the prober tests).
    const PUZZLE: &str = "\
        53..7....\
        6..195...\
        .98....6.\
        8...6...3\
        4..8.3..1\
        7...2...6\
        .6....28.\
        ...419..5\
        ....8..79";
    const SOLUTION: &str = "\
        534678912\
        672195348\
        198342567\
        859761423\
        426853791\
        713924856\
        961537284\
        287419635\
        345286179";

    fn grid(s: &str) -> DigitGrid {
        DigitGrid::parse(s).expect("valid grid")
    }

    #[test]
    fn solution_of_recovers_the_known_answer() {
        let sol = solution_of(&grid(PUZZLE)).expect("solvable");
        assert_eq!(sol.to_line(), grid(SOLUTION).to_line());
    }

    #[test]
    fn detects_empty_cell_contradiction() {
        // Clear one cell of the solution (its only candidate is the solution digit,
        // forced by all 20 peers), then strip that candidate: an empty cell with no
        // candidates (signature a).
        let mut p = grid(SOLUTION);
        p.clear(0);
        let mut b = Board::from_digits(&p);
        let d = grid(SOLUTION).get(0).unwrap();
        b.eliminate(0, d);
        assert!(is_inconsistent(&b));
    }

    #[test]
    fn detects_homeless_digit_contradiction() {
        // Clear one cell, then eliminate its solution digit from it: that digit now
        // has no home anywhere in the cell's row/col/box (signature b also fires).
        let mut p = grid(SOLUTION);
        p.clear(40); // a center cell
        let mut b = Board::from_digits(&p);
        let d = grid(SOLUTION).get(40).unwrap();
        b.eliminate(40, d);
        assert!(is_inconsistent(&b));
    }

    #[test]
    fn consistent_board_is_not_inconsistent() {
        assert!(!is_inconsistent(&Board::from_digits(&grid(PUZZLE))));
    }

    #[test]
    fn simple_moves_are_deduped_and_forced() {
        // The classic puzzle has naked/hidden singles available immediately.
        let b = Board::from_digits(&grid(PUZZLE));
        let mut moves = Vec::new();
        simple_moves(&b, &mut moves);
        assert!(!moves.is_empty(), "the classic puzzle has singles");
        // Each move is forced (its digit is a current candidate) and unique per cell.
        let sol = grid(SOLUTION);
        let mut cells_seen = std::collections::HashSet::new();
        for &(c, d) in &moves {
            assert!(b.marks(c).contains(d), "move places a candidate");
            assert_eq!(sol.get(c), Some(d), "forced move matches the solution");
            assert!(cells_seen.insert(c), "cell {c} appears once");
        }
    }
}
