//! Closure agreement: glab's logic engines (`solve::LogicSolver` and its fused fast
//! path `solve::FusedLogicSolver`) must reach the same `solved` outcome as
//! `sudoku_core`'s canonical solver (`solve_filtered`) — and as each other — on every
//! board the strip loop feeds the difficulty gate, for a spec's in-scope toolbox.
//! Core is the ground-truth reference the whole crate is kept faithful to; the
//! end-to-end spec VERDICT is cross-checked in `faithful.rs` (both directions, incl.
//! negatives). This test pins the one closure fact those engines must share per board.
//!
//! ## Why ONLY `solved`, and never the trace/counts
//!
//! We compare `solved` and nothing else. Per-kind firing counts (and the step trace)
//! are a property of one solve PATH, not of the puzzle: reorder the ladder and the
//! same outcome is reached via different steps, so the counts differ while every
//! engine is still correct. The `kind index is not a solve order` principle is
//! deliberate — each engine (and core's not-yet-optimized verifier) may try
//! techniques in whatever order is cheapest for it. Asserting counts here would pin a
//! trace and forfeit exactly that freedom; it only ever "passed" because glab's harder
//! ladder happened to mirror core's difficulty order.
//!
//! ## `solved` is invariant for THESE toolboxes — not in general
//!
//! Caveat, because it is not a free theorem: these techniques are NOT all monotone.
//! Several have non-monotone counting/positional preconditions ("d confined to ≥2
//! aligned cells", "N cells holding N candidates") that an earlier step can DESTROY,
//! so for an adversarially restricted toolbox solvability is genuinely path-dependent
//! (e.g. LC with hidden singles removed: a placement can shrink a box's candidates for
//! d to one cell — a hidden single, but it's excluded, so unplayed — which also
//! dissolves the LC pattern, losing an elimination one path made and the other didn't).
//! What rescues the real specs is the trunk: `train`/`drill` always allow Beginner +
//! Intermediate (hidden single, naked single, LC) IN FULL — the public `Spec` API
//! never strips them — so the witness redundancy that makes the closure converge is
//! always present. `solved`-agreement is therefore safe and meaningful for every spec
//! the generator can build; it is NOT something to rely on for hand-rolled toolboxes
//! that drop the trunk.
//!
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
use generator_lab::subset_spec_for_mode;
use generator_lab::spec::kinds::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, JELLYFISH, KindMask, LC_CLAIMING,
    LC_POINTING, NAKED_PAIR, NAKED_QUAD, NAKED_SINGLE, NAKED_TRIPLE, SWORDFISH, W_WING, X_WING,
    XYZ_WING, XY_WING,
};

use sudoku_core::{Board as CBoard, TechniqueKind as CK, solve_filtered};

/// core `TechniqueKind` -> glab kind index, for the modelled ladder (Trunk + Subset
/// + Fish + Bivalue branches). Used to restrict core's solver to the spec's toolbox:
/// a core kind is allowed iff it maps to a glab kind whose bit is set. `None` for the
/// techniques glab does not model (turbot/finned fish, ALS, ring) — no generated spec
/// allows them, so excluding them from core matches glab's toolbox exactly.
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
        CK::XWing => X_WING,
        CK::Swordfish => SWORDFISH,
        CK::Jellyfish => JELLYFISH,
        CK::XYWing => XY_WING,
        CK::XYZWing => XYZ_WING,
        CK::WWing => W_WING,
        _ => return None,
    })
}

/// The reference verdict: whether core's canonical solver, restricted to the
/// baseline's kinds, solves the board. A core kind is allowed iff it maps to a glab
/// kind whose bit is set in `baseline`. Only `solved` is read — the trace is a path
/// property we deliberately don't compare (see module docs).
fn core_solved(line: &str, baseline: KindMask) -> bool {
    let board = CBoard::parse(line).expect("core parse");
    solve_filtered(board, |ck| glab_idx(ck).is_some_and(|idx| baseline & (1 << idx) != 0)).solved
}

/// Whether the composable logic solver on the banded packing solves the board — the
/// same puzzle the reference sees.
fn logic_solved(line: &str, baseline: KindMask) -> bool {
    let grid = DigitGrid::parse(line).expect("valid line");
    let state = SolverState::<Bands<RowMajor>>::from_digits(&grid);
    LogicSolver::solve_tracked(&state, baseline).solved
}

/// Whether the fused fast-path logic solver on the dual-banded grid solves it.
fn fused_solved(line: &str, baseline: KindMask) -> bool {
    let grid = DigitGrid::parse(line).expect("valid line");
    let dual = DualSolverState::from_digits(&grid);
    FusedLogicSolver::solve_tracked(&dual, baseline).solved
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

            // Both new engines and the core reference, on the identical board: they
            // must agree on the order-invariant `solved` verdict (the closure fact;
            // see module docs for why we compare nothing else).
            let line = cb.to_line();
            let core = core_solved(&line, baseline);
            let logic = logic_solved(&line, baseline);
            let fused = fused_solved(&line, baseline);
            compared += 1;

            assert_eq!(logic, core, "[{label}] logic solved mismatch vs core on {line}");
            assert_eq!(fused, core, "[{label}] fused solved mismatch vs core on {line}");

            // Follow the core verdict to stay on the real trajectory.
            if !core {
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
    check_spec("train", &subset_spec_for_mode(0), 400, 1000);
}

#[test]
fn logic_matches_baselines_drill() {
    check_spec("drill", &subset_spec_for_mode(1), 400, 1000);
}

/// Fish branch (single-digit): the engines must agree with core on `solved` for the
/// X-Wing toolbox too. The comparison runs on every kept strip board (not only fish
/// boards), so the 1000-board floor is met regardless of how often the fish fires.
#[test]
fn logic_matches_fish_train_xwing() {
    check_spec("train-xwing", &Spec::train(X_WING), 400, 1000);
}

#[test]
fn logic_matches_fish_drill_xwing() {
    check_spec("drill-xwing", &Spec::drill(X_WING), 400, 1000);
}

/// Bivalue-chain branch (wings): same `solved`-agreement for the XY-Wing toolbox,
/// exercising the new wing bodies' closure against core on real strip boards.
#[test]
fn logic_matches_bivalue_train_xywing() {
    check_spec("train-xywing", &Spec::train(XY_WING), 400, 1000);
}

#[test]
fn logic_matches_bivalue_drill_xywing() {
    check_spec("drill-xywing", &Spec::drill(XY_WING), 400, 1000);
}
