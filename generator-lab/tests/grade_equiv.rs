//! The campaign grader's instrumented solve ([`solve_graded`]) must be a faithful twin of
//! the production easiest-first solve ([`LogicSolver::solve_tracked`]): same `solved`
//! verdict and the same sequence of *harder* steps (kinds `>= NAKED_PAIR`), because it
//! walks the identical cheap-closure fixpoints and the identical `HARD_STEPS_DEFAULT`
//! ladder. We assert exactly that on a broad spread of partial boards.
//!
//! ## What we compare, and what we don't
//!
//! Only `solved` and the HARDER per-kind counts. The cheap counts (singles + locked
//! candidates) are a path property — the grading closure drains them in a different
//! interleaving than `solve_tracked` — so they legitimately differ and are never read by
//! the grader (see `solve::CHEAP_KINDS`). The harder counts DO match because both engines
//! take harder steps only from the singles+LC fixpoint, in the same order. This is the
//! same "compare the order-invariant fact, not the path" discipline `logic_equiv.rs` uses
//! across engines.

use generator_lab::fill::random_solution;
use generator_lab::generate::{AttemptResult, attempt};
use generator_lab::grade::{Weights, grade_node};
use generator_lab::repr::banded::{Bands, RowMajor};
use generator_lab::repr::{CELLS, DigitGrid, Marks, SolverState};
use generator_lab::rng::Rng;
use generator_lab::solve::{LogicSolver, Solver, solve_graded};
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{HIDDEN_QUAD, NAKED_PAIR, NUM, W_WING, X_WING, XY_WING};

type Board = SolverState<Bands<RowMajor>>;

/// Compare the grading solve against `solve_tracked` on `board` for `baseline`: the
/// `solved` verdict and every harder-kind (`>= NAKED_PAIR`) count must agree.
fn assert_twin(board: &Board, baseline: u32, label: &str, line: &str) {
    let tracked = LogicSolver::solve_tracked(board, baseline);
    let graded = solve_graded(board, baseline);
    assert_eq!(graded.solved, tracked.solved, "[{label}] solved mismatch on {line}");
    assert_eq!(
        &graded.counts[NAKED_PAIR..NUM],
        &tracked.counts[NAKED_PAIR..NUM],
        "[{label}] harder-count mismatch on {line}",
    );
    // The recorded steps are exactly the harder firings, in order — cross-check the step
    // list against the counts it claims to summarize.
    let mut from_steps = [0u16; NUM];
    for s in &graded.steps {
        from_steps[s.kind as usize] += 1;
    }
    assert_eq!(
        &from_steps[NAKED_PAIR..NUM],
        &graded.counts[NAKED_PAIR..NUM],
        "[{label}] step list disagrees with counts on {line}",
    );
}

/// Across many random solutions, carve diverse partial boards (remove a varying number of
/// clues) and assert the grading solve twins `solve_tracked` for several toolboxes. The
/// boards need not be unique/valid puzzles — both engines run on the identical board, so
/// agreement must hold regardless.
#[test]
fn graded_twins_tracked_on_partial_boards() {
    let specs = [
        ("train-hidden-quad", Spec::train(HIDDEN_QUAD)),
        ("drill-hidden-quad", Spec::drill(HIDDEN_QUAD)),
        ("train-x-wing", Spec::train(X_WING)),
        ("train-xy-wing", Spec::train(XY_WING)),
        ("train-w-wing", Spec::train(W_WING)),
    ];
    let mut rng = Rng::from_seed(7);
    let mut compared = 0usize;
    for _ in 0..120 {
        let solution = random_solution(&mut rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        // A handful of removal depths per solution: from nearly full (cheap-solvable) to
        // sparse (stalls early), so subsets, fish, and wings all get exercised.
        for &remove in &[25usize, 35, 45, 55] {
            let mut digits: DigitGrid = solution.0.clone();
            for &cell in &positions[..remove] {
                digits.clear(cell);
            }
            let board = Board::from_digits(&digits);
            let line = digits.to_line();
            for (label, spec) in &specs {
                assert_twin(&board, spec.baseline_mask(), label, &line);
                compared += 1;
            }
        }
    }
    assert!(compared > 2000, "too few comparisons ({compared}) — test too weak");
    eprintln!("graded twins tracked on {compared} (board, spec) pairs");
}

/// End-to-end: grade a real node's yield. Generate Naked-Pair train puzzles (a cheap
/// Expert target), band them, and check the wiring holds: every band index is valid, the
/// batch fills all three bands, and every yielded puzzle actually hit its bottleneck (the
/// forced Naked Pair fired on the easiest-first path) and solved.
#[test]
fn grade_node_bands_a_real_yield() {
    let spec = Spec::train(NAKED_PAIR);
    let mut puzzles: Vec<DigitGrid> = Vec::new();
    let mut seed = 1u64;
    while puzzles.len() < 60 && seed < 5000 {
        let mut rng = Rng::from_seed(seed);
        if let AttemptResult::Success(p) = attempt(&mut rng, &spec) {
            puzzles.push(p.puzzle.0);
        }
        seed += 1;
    }
    assert!(puzzles.len() >= 30, "too few Naked-Pair yields ({})", puzzles.len());

    let n_bands = 3;
    let graded = grade_node(&spec, &puzzles, &Weights::default(), n_bands);
    assert_eq!(graded.len(), puzzles.len());

    let mut band_counts = [0usize; 3];
    for g in &graded {
        assert!(g.band < n_bands, "band {} out of range", g.band);
        assert!(g.signals.bottleneck_count >= 1, "a forced Naked-Pair yield must hit T");
        assert!(g.signals.scarcity < u32::MAX, "a bottleneck puzzle has a finite scarcity");
        band_counts[g.band] += 1;
    }
    assert!(band_counts.iter().all(|&c| c > 0), "all three bands populated, got {band_counts:?}");
    eprintln!("graded {} Naked-Pair puzzles into bands {band_counts:?}", puzzles.len());
}
