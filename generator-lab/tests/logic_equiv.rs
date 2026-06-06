//! Equivalence: the new-representation logic solvers (`solve::LogicSolver` and its
//! fused fast path `solve::FusedLogicSolver`) must agree with the proven scalar
//! reference engine `techniques::solve_tracked` — and with each other — on every
//! board the strip loop feeds the difficulty gate, for the two production specs.
//!
//! The contract the generator relies on:
//!   - same `solved` verdict,
//!   - same `requirement_met` verdict (the strip reads only the Forced kinds' counts),
//!   - identical subset counts (NakedPair..HiddenQuad).
//!
//! The cheap-kind counts (singles, the two locked-candidate orientations) are NOT
//! compared per-kind: the bb fast path's fused closure reorders them, so their
//! per-kind fired-or-not is not order-invariant. The subset counts MUST match
//! because all three engines reach the identical {singles, locked-candidates}
//! fixpoint before any subset can fire. Walking REAL strip trajectories makes the
//! boards exactly the post-uniqueness-gate boards the gate sees in production.

use generator_lab::generator::random_full_grid;
use generator_lab::grid::{Board, CELLS, digit_to_bit};
use generator_lab::probe::{Prober, Search};
use generator_lab::repr::banded::{Bands, DualBandedMarkGrid, RowMajor};
use generator_lab::repr::{DigitGrid, Marks, SearchState};
use generator_lab::rng::Rng;
use generator_lab::scan::Bivalue;
use generator_lab::solve::{FusedLogicSolver, LogicSolver, Solver};
use generator_lab::spec::Spec;
use generator_lab::spec_for_mode;
use generator_lab::technique_kinds::{NAKED_PAIR, NUM, SolveTrace};
use generator_lab::techniques::solve_tracked;

/// The composable logic solver on the banded packing, derived from the scalar
/// board's line — the same puzzle the reference engines see.
fn logic_trace(puzzle: &Board, baseline: u32) -> SolveTrace {
    let grid = DigitGrid::parse(&puzzle.to_line()).expect("valid line");
    let state = SearchState::<Bands<RowMajor>>::from_digits(&grid);
    LogicSolver::solve_tracked(&state, baseline)
}

/// The fused fast-path logic solver on the dual-banded grid, same puzzle.
fn fused_trace(puzzle: &Board, baseline: u32) -> SolveTrace {
    let grid = DigitGrid::parse(&puzzle.to_line()).expect("valid line");
    let dual = DualBandedMarkGrid::from_digits(&grid);
    FusedLogicSolver::solve_tracked(&dual, baseline)
}

fn check_spec(label: &str, spec: &Spec, attempts: usize, min_compared: usize) {
    let baseline = spec.baseline_mask();
    let mut rng = Rng::from_seed(1);
    let mut compared = 0usize;

    for _ in 0..attempts {
        let solution = random_full_grid(&mut rng);
        let mut puzzle = solution.clone();
        let mut positions: Vec<usize> = (0..CELLS).collect();
        rng.shuffle(&mut positions);

        for i in positions {
            if puzzle.is_empty(i) {
                continue;
            }
            let orig = puzzle.cell(i);
            puzzle.clear_naked(i);

            // Uniqueness gate on the new prober stack (bb's `any_alt_solves`): forbid
            // `orig` at `i` to restrict the cell to its alternates and ask a single
            // existence query. Non-unique => revert and stay on the real trajectory.
            let alts = puzzle.candidates(i) & !digit_to_bit(orig);
            let nonunique = alts != 0 && {
                let grid = DigitGrid::parse(&puzzle.to_line()).expect("valid line");
                let mut probe = SearchState::<Bands<RowMajor>>::from_digits(&grid);
                probe.forbid(i, generator_lab::repr::Digit::new(orig).expect("nonzero clue digit"));
                Search::<Bivalue>::has_completion(probe)
            };
            if nonunique {
                puzzle.place(i, orig);
                continue;
            }

            // Both new engines and the scalar reference, on the identical board.
            let scal = solve_tracked(&puzzle, baseline);
            let logic = logic_trace(&puzzle, baseline);
            let fused = fused_trace(&puzzle, baseline);
            compared += 1;

            let line = puzzle.to_line();
            for (new, nname) in [(&logic, "logic"), (&fused, "fused")] {
                for (other, oname) in [(&scal, "scalar")] {
                    assert_eq!(
                        new.solved, other.solved,
                        "[{label}] {nname} solved mismatch vs {oname} on {line}"
                    );
                    assert_eq!(
                        spec.requirement_met(&new.counts),
                        spec.requirement_met(&other.counts),
                        "[{label}] {nname} requirement_met mismatch vs {oname} ({:?} vs {:?}) on {line}",
                        new.counts,
                        other.counts
                    );
                    for k in NAKED_PAIR..NUM {
                        assert_eq!(
                            new.counts[k], other.counts[k],
                            "[{label}] {nname} subset kind {k} count mismatch vs {oname} \
                             ({} vs {}) on {line}",
                            new.counts[k], other.counts[k]
                        );
                    }
                }
            }

            // Follow the scalar verdict to stay on the real trajectory.
            if !scal.solved {
                puzzle.place(i, orig);
            }
        }
    }
    assert!(
        compared > min_compared,
        "[{label}] too few comparisons ({compared}) — test too weak"
    );
    eprintln!("[{label}] {compared} board comparisons agreed");
}

#[test]
fn logic_matches_baselines_train() {
    check_spec("train", &spec_for_mode(0), 400, 1000);
}

#[test]
fn logic_matches_baselines_drill() {
    check_spec("drill", &spec_for_mode(1), 400, 1000);
}
