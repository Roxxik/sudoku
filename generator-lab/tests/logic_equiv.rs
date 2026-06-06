//! Equivalence: the new-representation logic solvers (`solve::LogicSolver` and its
//! fused fast path `solve::FusedLogicSolver`) must agree with `sudoku_core`'s
//! canonical solver (`solve_filtered`) — and with each other — on every board the
//! strip loop feeds the difficulty gate, for the two production specs. Core is the
//! ground-truth reference the whole crate is kept faithful to (see `faithful.rs`);
//! here it pins the per-kind subset COUNTS the strip gate reads, which core's bool
//! `verify` can't expose.
//!
//! The contract the generator relies on:
//!   - same `solved` verdict,
//!   - same `requirement_met` verdict (the strip reads only the Forced kinds' counts),
//!   - identical subset counts (NakedPair..HiddenQuad).
//!
//! The cheap-kind counts (singles, the two locked-candidate orientations) are NOT
//! compared per-kind: glab's fused fast path reorders them, and glab batches naked
//! singles where core fires one per step, so their per-kind fired count is not
//! order-invariant. The subset counts MUST match because every engine reaches the
//! identical {singles, locked-candidates} fixpoint before any subset can fire, and
//! both core and glab then apply one subset step at a time in difficulty order.
//! Walking REAL strip trajectories — on a core board, gated by glab's prober — makes
//! the boards exactly the post-uniqueness-gate boards the gate sees in production.

use generator_lab::fill::random_solution;
use generator_lab::probe::{Prober, Search};
use generator_lab::repr::banded::{Bands, DualSolverState, RowMajor};
use generator_lab::repr::{CELLS, Digit, DigitGrid, Marks, SolverState};
use generator_lab::rng::Rng;
use generator_lab::scan::Bivalue;
use generator_lab::solve::{FusedLogicSolver, LogicSolver, Solver};
use generator_lab::spec::Spec;
use generator_lab::spec_for_mode;
use generator_lab::spec::kinds::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, KindMask, LC_CLAIMING, LC_POINTING,
    NAKED_PAIR, NAKED_QUAD, NAKED_SINGLE, NAKED_TRIPLE, NUM, SolveTrace,
};

use sudoku_core::{Board as CBoard, TechniqueKind as CK, solve_filtered};

/// core `TechniqueKind` -> glab kind index, for the PoC ladder. `None` for the
/// harder techniques glab does not model (fish/wings/ring) — the baseline never
/// allows them, so they never appear in a trace here.
fn glab_idx(ck: CK) -> Option<usize> {
    Some(match ck {
        CK::NakedSingle => NAKED_SINGLE,
        CK::HiddenSingle => HIDDEN_SINGLE,
        CK::LockedCandidatesPointing => LC_POINTING,
        CK::LockedCandidatesClaiming => LC_CLAIMING,
        CK::NakedPair => NAKED_PAIR,
        CK::HiddenPair => HIDDEN_PAIR,
        CK::NakedTriple => NAKED_TRIPLE,
        CK::HiddenTriple => HIDDEN_TRIPLE,
        CK::NakedQuad => NAKED_QUAD,
        CK::HiddenQuad => HIDDEN_QUAD,
        _ => return None,
    })
}

/// The reference trace: core's canonical solver restricted to the baseline's kinds,
/// reduced to glab's [`SolveTrace`] shape (solved + per-kind fire counts). A kind is
/// allowed iff its glab bit is set in `baseline`; counting trace steps per kind
/// gives the same per-subset counts glab's one-step-at-a-time solver produces.
fn core_trace(line: &str, baseline: KindMask) -> SolveTrace {
    let board = CBoard::parse(line).expect("core parse");
    let res = solve_filtered(board, |ck| {
        glab_idx(ck).is_some_and(|idx| baseline & (1 << idx) != 0)
    });
    let mut counts = [0u16; NUM];
    for step in &res.trace {
        if let Some(idx) = glab_idx(step.technique) {
            counts[idx] = counts[idx].saturating_add(1);
        }
    }
    SolveTrace { solved: res.solved, counts }
}

/// The composable logic solver on the banded packing, from the board's line — the
/// same puzzle the reference sees.
fn logic_trace(line: &str, baseline: KindMask) -> SolveTrace {
    let grid = DigitGrid::parse(line).expect("valid line");
    let state = SolverState::<Bands<RowMajor>>::from_digits(&grid);
    LogicSolver::solve_tracked(&state, baseline)
}

/// The fused fast-path logic solver on the dual-banded grid, same puzzle.
fn fused_trace(line: &str, baseline: KindMask) -> SolveTrace {
    let grid = DigitGrid::parse(line).expect("valid line");
    let dual = DualSolverState::from_digits(&grid);
    FusedLogicSolver::solve_tracked(&dual, baseline)
}

fn check_spec(label: &str, spec: &Spec, attempts: usize, min_compared: usize) {
    let baseline = spec.baseline_mask();
    let mut rng = Rng::from_seed(1);
    let mut compared = 0usize;

    for _ in 0..attempts {
        // Same fill + RNG stream as production; the trajectory board is a core
        // board (it owns the strip API — clear_naked/candidates — and is the
        // reference), gated by glab's prober.
        let solution = random_solution(&mut rng);
        let mut cb = CBoard::parse(&solution.0.to_line()).expect("core parse solution");
        let mut positions: Vec<usize> = (0..CELLS).collect();
        rng.shuffle(&mut positions);

        for i in positions {
            if cb.is_empty(i) {
                continue;
            }
            let orig = cb.cell(i);
            cb.clear_naked(i);

            // Uniqueness gate on the new prober stack (bb's `any_alt_solves`): forbid
            // `orig` at `i` to restrict the cell to its alternates and ask a single
            // existence query. Non-unique => revert and stay on the real trajectory.
            let alts = cb.candidates(i) & !(1u16 << (orig - 1));
            let nonunique = alts != 0 && {
                let grid = DigitGrid::parse(&cb.to_line()).expect("valid line");
                let mut probe = SolverState::<Bands<RowMajor>>::from_digits(&grid);
                probe.forbid(i, Digit::new(orig).expect("nonzero clue digit"));
                Search::<Bivalue>::has_completion(probe)
            };
            if nonunique {
                cb.place(i, orig);
                continue;
            }

            // Both new engines and the core reference, on the identical board.
            let line = cb.to_line();
            let core = core_trace(&line, baseline);
            let logic = logic_trace(&line, baseline);
            let fused = fused_trace(&line, baseline);
            compared += 1;

            for (new, nname) in [(&logic, "logic"), (&fused, "fused")] {
                assert_eq!(
                    new.solved, core.solved,
                    "[{label}] {nname} solved mismatch vs core on {line}"
                );
                assert_eq!(
                    spec.requirement_met(&new.counts),
                    spec.requirement_met(&core.counts),
                    "[{label}] {nname} requirement_met mismatch vs core ({:?} vs {:?}) on {line}",
                    new.counts,
                    core.counts
                );
                for k in NAKED_PAIR..NUM {
                    assert_eq!(
                        new.counts[k], core.counts[k],
                        "[{label}] {nname} subset kind {k} count mismatch vs core \
                         ({} vs {}) on {line}",
                        new.counts[k], core.counts[k]
                    );
                }
            }

            // Follow the core verdict to stay on the real trajectory.
            if !core.solved {
                cb.place(i, orig);
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
