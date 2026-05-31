//! The spec-driven `random`-method generator — core's
//! `make_puzzle_for_spec_with_search` with the `random` path ONLY (no local
//! search, no construction) and none of the play-time bookkeeping.
//!
//! Per attempt: a random full grid, then strip cells in random order keeping a
//! strip iff the puzzle stays unique (prober) AND baseline-solvable (spec
//! toolbox). The most-stripped state whose baseline trace meets the requirement
//! counts is remembered as `best`; after the strip, if a `best` exists and it
//! passes [`verify`], the attempt succeeds. This is the exact gate sequence and
//! `best`/requirement/verify logic from core, just stripped to bools + counts.

use crate::bb::BitBoard;
use crate::grid::{Board, CELLS, digit_to_bit, iter_digits, popcount};
use crate::rng::Rng;
use crate::spec::Spec;
use crate::verify::verify;

/// A random complete solution grid. Same MRV+shuffle fill as core.
#[cfg_attr(feature = "profiling", inline(never))]
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

/// A generated puzzle and the full solution it was stripped from.
pub struct GeneratedPuzzle {
    pub puzzle: Board,
    pub solution: Board,
    pub givens: usize,
}

/// Why a single attempt ended.
pub enum Outcome {
    /// A puzzle satisfying the spec (passed verify).
    Success(GeneratedPuzzle),
    /// A requirement-meeting `best` was found but verify rejected it (the target
    /// was substitutable). Core's `requirement_not_forced`.
    NotForced,
    /// No strip ever met the requirement counts. Core's `requirement_never_fired`.
    NeverFired,
}

/// One full strip attempt for `spec`. Mirrors core's per-attempt body.
pub fn attempt(rng: &mut Rng, spec: &Spec) -> Outcome {
    let baseline = spec.baseline_mask();
    let solution = random_full_grid(rng);
    let mut puzzle = solution.clone();
    let mut positions: Vec<usize> = (0..CELLS).collect();
    rng.shuffle(&mut positions);

    // The shared bitboard is maintained incrementally across the strip: a
    // clear_naked/place only touches cell i and its peers, so we refresh those
    // 21 cells instead of rebuilding all 81 (`from_board`) each position.
    let mut bb = BitBoard::from_board(&puzzle);

    let mut best: Option<Board> = None;
    for i in positions {
        if puzzle.is_empty(i) {
            continue;
        }
        let orig = puzzle.cell(i);
        puzzle.clear_naked(i);
        bb.apply_clear(&puzzle, i, orig);
        debug_assert!(bb == BitBoard::from_board(&puzzle), "bb desync after clear at {i}");

        // Uniqueness gate.
        let v_bit = digit_to_bit(solution.cell(i));
        let alts = puzzle.candidates(i) & !v_bit;
        if alts != 0 && bb.any_alt_solves(i, alts) {
            puzzle.place(i, orig);
            bb.apply_place(i, orig);
            debug_assert!(bb == BitBoard::from_board(&puzzle), "bb desync after revert at {i}");
            continue;
        }

        // Baseline gate + requirement counts (one tracked solve).
        let outcome = bb.baseline(baseline);
        if !outcome.solved {
            puzzle.place(i, orig);
            bb.apply_place(i, orig);
            debug_assert!(bb == BitBoard::from_board(&puzzle), "bb desync after revert at {i}");
            continue;
        }
        if spec.requirement_met(&outcome.counts) {
            best = Some(puzzle.clone());
        }
        // Accept the strip: `puzzle` and `bb` stay in the stripped state.
    }

    match best {
        Some(seed) => {
            if verify(&seed, spec) {
                let givens = seed.givens();
                Outcome::Success(GeneratedPuzzle {
                    puzzle: seed,
                    solution,
                    givens,
                })
            } else {
                Outcome::NotForced
            }
        }
        None => Outcome::NeverFired,
    }
}

/// Per-run tallies, for throughput/yield reporting.
#[derive(Default, Clone, Copy)]
pub struct GenStats {
    pub attempts: usize,
    pub successes: usize,
    pub never_fired: usize,
    pub not_forced: usize,
    pub total_givens: usize,
}

/// Generate until a puzzle satisfying `spec` is found or `max_attempts` is hit.
/// Returns the puzzle (if any) and the tallies up to that point.
pub fn generate(rng: &mut Rng, spec: &Spec, max_attempts: usize) -> (Option<GeneratedPuzzle>, GenStats) {
    let mut stats = GenStats::default();
    for _ in 0..max_attempts {
        stats.attempts += 1;
        match attempt(rng, spec) {
            Outcome::Success(p) => {
                stats.successes += 1;
                stats.total_givens += p.givens;
                return (Some(p), stats);
            }
            Outcome::NotForced => stats.not_forced += 1,
            Outcome::NeverFired => stats.never_fired += 1,
        }
    }
    (None, stats)
}

/// Run exactly `n` attempts (NOT "until found") for `spec` — a fixed-work,
/// deterministic benchmark of the per-attempt cost mix. Returns the tallies plus
/// a fingerprint over produced puzzles (prevents dead-code elimination and lets
/// native/wasm cross-check they did identical work).
pub fn run_attempts(rng: &mut Rng, spec: &Spec, n: usize) -> (GenStats, u64) {
    let mut stats = GenStats::default();
    let mut fp: u64 = 0xcbf29ce484222325;
    for _ in 0..n {
        stats.attempts += 1;
        match attempt(rng, spec) {
            Outcome::Success(p) => {
                stats.successes += 1;
                stats.total_givens += p.givens;
                for i in 0..CELLS {
                    fp ^= p.puzzle.cell(i) as u64;
                    fp = fp.wrapping_mul(0x100000001b3);
                }
            }
            Outcome::NotForced => {
                stats.not_forced += 1;
                fp = fp.wrapping_mul(0x100000001b3);
            }
            Outcome::NeverFired => {
                stats.never_fired += 1;
                fp = fp.wrapping_mul(0x100000001b3);
            }
        }
    }
    (stats, fp)
}
