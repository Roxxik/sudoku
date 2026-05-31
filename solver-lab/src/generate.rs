//! The `train(HiddenQuad)` generator's hot loop, ported from
//! `core::generator::make_puzzle_for_spec_with_search` (the `random` method,
//! i.e. no local search, no construction), generic over the existence prober.
//!
//! What it keeps (the parts that touch the prober): random full grid, random
//! strip order, the per-cell uniqueness gate (prober) and the baseline-solvable
//! gate. What it drops (solver-independent, only decides *when an attempt
//! succeeds*, never the strip trajectory): requirement counting, `best`
//! tracking, and `verify`. See the module-level reasoning in `lib.rs`.

use crate::grid::{Board, CELLS, digit_to_bit, iter_digits, popcount};
use crate::rng::Rng;
use crate::solvers::UniqProber;
use crate::techniques::{baseline_outcome, baseline_solvable};

/// A random complete solution grid. Copied from `core::generator`.
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

/// Outcome of one strip attempt: the stripped puzzle and its (unique) solution.
pub struct Attempt {
    pub puzzle: Board,
    pub solution: Board,
}

/// Run one full strip attempt with prober `P`: take a fresh random grid, strip
/// cells in random order, keeping a strip iff the puzzle stays unique (prober)
/// AND baseline-solvable. Identical to core's strip loop minus the
/// solver-independent success bookkeeping.
///
/// The prober answers must match the oracle, so for a given `rng` state every
/// correct prover walks the identical trajectory and produces the identical
/// puzzle — that is what makes the cross-prover timing comparison fair.
pub fn strip_attempt<P: UniqProber>(rng: &mut Rng) -> Attempt {
    let solution = random_full_grid(rng);
    let mut puzzle = solution.clone();
    let mut positions: Vec<usize> = (0..CELLS).collect();
    rng.shuffle(&mut positions);

    for i in positions {
        if puzzle.is_empty(i) {
            continue;
        }
        let orig = puzzle.cell(i);
        puzzle.clear_naked(i);

        // Uniqueness gate: the pre-strip board was unique with solution
        // `solution`, so a second solution must differ at `i`. The board is
        // non-unique iff some *alternate* digit at `i` also completes.
        let v_bit = digit_to_bit(solution.cell(i));
        let alts = puzzle.candidates(i) & !v_bit;
        let mut non_unique = false;
        if alts != 0 {
            let mut probe = P::from_board(&puzzle);
            non_unique = probe.any_alt_solves(i, alts);
        }
        if non_unique {
            puzzle.place(i, orig); // reject the strip
            continue;
        }

        // Baseline gate: keep the strip only if still solvable by the
        // up-to-HiddenQuad toolbox.
        if !baseline_solvable(&puzzle) {
            puzzle.place(i, orig);
            continue;
        }
        // Accept: `puzzle` stays in its stripped state.
    }

    Attempt { puzzle, solution }
}

/// One strip attempt for the `find_quad` workload — the "real deal" the
/// microbenchmark ([`strip_attempt`]) approximates. Identical strip loop, but it
/// also reproduces core's `best`/`requirement_met` tracking: it remembers the
/// most-stripped accepted state whose baseline trace *uses* a hidden quad, and
/// returns it (or `None` if this attempt never produced one).
///
/// Same prober workload as [`strip_attempt`], so timing this against the bench
/// shows how much the extra `best`/`baseline_outcome` bookkeeping costs on top
/// of the prober that both share.
pub fn quad_attempt<P: UniqProber>(rng: &mut Rng) -> Option<Board> {
    let solution = random_full_grid(rng);
    let mut puzzle = solution.clone();
    let mut positions: Vec<usize> = (0..CELLS).collect();
    rng.shuffle(&mut positions);
    let mut best: Option<Board> = None;
    for i in positions {
        if puzzle.is_empty(i) {
            continue;
        }
        let orig = puzzle.cell(i);
        puzzle.clear_naked(i);
        let v_bit = digit_to_bit(solution.cell(i));
        let alts = puzzle.candidates(i) & !v_bit;
        let mut non_unique = false;
        if alts != 0 {
            let mut probe = P::from_board(&puzzle);
            non_unique = probe.any_alt_solves(i, alts);
        }
        if non_unique {
            puzzle.place(i, orig);
            continue;
        }
        let (solved, used_hq) = baseline_outcome(&puzzle);
        if !solved {
            puzzle.place(i, orig);
            continue;
        }
        if used_hq {
            best = Some(puzzle.clone());
        }
    }
    best
}
