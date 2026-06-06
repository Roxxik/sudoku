//! Correctness anchor for the packed SIMT baseline solver
//! ([`solve::simt::PackedSolver`]): on the faithful corpus of baseline-gate boards a
//! real generator run produces ([`generate::random_simt::collect_baseline_boards`]),
//! the SIMT solver's verdict must match the scalar
//! [`FusedLogicSolver`](generator_lab::solve::FusedLogicSolver) the SIMT codepath
//! replaces — the same `solved` flag and the same subset-kind counts the spec's
//! requirement check reads.
//!
//! Scope: both production specs. **drill(HiddenQuad)** baseline is the no-LC cheap
//! closure (naked + hidden singles, columns included) + the scalar HiddenQuad fallback;
//! **train(HiddenQuad)** additionally exercises the vectorized locked-candidates round
//! and the full subset ladder. Both must match the scalar engine on `solved` and the
//! subset-kind counts the requirement check reads.

use generator_lab::generate::random_simt::collect_baseline_boards;
use generator_lab::repr::banded::DualSolverState;
use generator_lab::repr::Marks;
use generator_lab::solve::simt::{PackedSolver, SolveQuery};
use generator_lab::solve::{FusedLogicSolver, LogicSolver, Solver};
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{NAKED_PAIR, NUM, SolveTrace};
use generator_lab::spec_for_mode;

/// Compare PackedSolver against FusedLogicSolver (and the composable LogicSolver) on
/// every harvested board, for the given `spec`, exercising BOTH locked-candidates
/// placements: off the warp (the default) and vectorized in the closure (kept for
/// research). Both must match the scalar engine, so neither path bitrots.
fn check_against_scalar(spec: &Spec, base_seed: u64, lanes: usize, attempts_per_lane: usize) {
    for lc_in_closure in [false, true] {
        check_lc(spec, base_seed, lanes, attempts_per_lane, lc_in_closure);
    }
}

fn check_lc(spec: &Spec, base_seed: u64, lanes: usize, attempts_per_lane: usize, lc_in_closure: bool) {
    let baseline = spec.baseline_mask();
    let boards = collect_baseline_boards(base_seed, spec, lanes, attempts_per_lane);
    assert!(!boards.is_empty(), "harvested an empty baseline corpus");

    let queries: Vec<SolveQuery> = boards.iter().map(SolveQuery::from_digits).collect();
    let mut out = vec![SolveTrace::default(); queries.len()];
    PackedSolver::new().solve_with(baseline, lc_in_closure, &queries, &mut out);

    let mut solved_count = 0usize;
    let mut hq_count = 0usize;
    for (i, g) in boards.iter().enumerate() {
        let dual = DualSolverState::from_digits(g);
        let reference = FusedLogicSolver::solve_tracked(&dual, baseline);
        // The composable engine is the independent oracle for `solved`.
        let composable = LogicSolver::solve_tracked(&dual, baseline);
        let mine = out[i];

        assert_eq!(
            mine.solved, reference.solved,
            "board {i}: solved (mine {} vs fused {})",
            mine.solved, reference.solved
        );
        assert_eq!(
            reference.solved, composable.solved,
            "board {i}: fused vs composable solved (oracle disagreement, not my bug)"
        );
        // Subset-kind counts (the ones the requirement check reads) must match exactly.
        for k in NAKED_PAIR..NUM {
            assert_eq!(
                mine.counts[k], reference.counts[k],
                "board {i}: counts[{k}] (mine {} vs fused {})",
                mine.counts[k], reference.counts[k]
            );
        }
        // The actual gate the warp reads.
        assert_eq!(
            spec.requirement_met(&mine.counts),
            spec.requirement_met(&reference.counts),
            "board {i}: requirement_met"
        );

        if mine.solved {
            solved_count += 1;
        }
        if spec.requirement_met(&mine.counts) {
            hq_count += 1;
        }
    }
    // Sanity that the corpus actually exercised the paths (solved-heavy, some HiddenQuad).
    eprintln!(
        "baseline corpus (lc_in_closure={lc_in_closure}): {} boards, {} solved, {} requirement-met",
        boards.len(),
        solved_count,
        hq_count
    );
    assert!(solved_count > 0, "no board solved — corpus or solver is degenerate");
}

#[test]
fn drill_baseline_simt_matches_scalar() {
    check_against_scalar(&spec_for_mode(1), 1, 8, 150);
}

#[test]
fn train_baseline_simt_matches_scalar() {
    check_against_scalar(&spec_for_mode(0), 1, 8, 150);
}

/// A different seed base + uneven lane lengths, to shake out streaming/refill races in
/// the per-lane count accumulation.
#[test]
fn drill_baseline_simt_offset_seeds() {
    check_against_scalar(&spec_for_mode(1), 1000, 13, 60);
}

#[test]
fn train_baseline_simt_offset_seeds() {
    check_against_scalar(&spec_for_mode(0), 1000, 13, 60);
}
