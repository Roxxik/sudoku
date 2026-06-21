//! End-to-end coverage for the disjunctive `force_any` constraint: a generated puzzle
//! must require *some* technique from the set (be unsolvable without it) while leaving
//! the choice of member open. A single `force` is the degenerate one-member case, and
//! forcing a specific member implies the disjunction.

use generator_lab::generate::{generate, verify};
use generator_lab::repr::Board;
use generator_lab::rng::Rng;
use generator_lab::solve::{LogicSolver, Solver};
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{
    HIDDEN_PAIR, HIDDEN_SINGLE, KindMask, LC_CLAIMING, LC_POINTING, NAKED_PAIR, NAKED_SINGLE,
};

fn mask(kinds: &[usize]) -> KindMask {
    kinds.iter().fold(0, |m, &k| m | (1 << k))
}

/// Singles + both locked candidates — the "easy" toolbox sitting below the pair set.
fn singles_lc() -> Spec {
    Spec::explicit().allow(NAKED_SINGLE).allow(HIDDEN_SINGLE).allow(LC_POINTING).allow(LC_CLAIMING)
}

const PAIRS: &[usize] = &[NAKED_PAIR, HIDDEN_PAIR];

/// A `force_any({NakedPair, HiddenPair})` puzzle solves with the full baseline but NOT
/// with singles + LC alone — i.e. some pair is genuinely required, which is exactly what
/// the disjunction promises (and what `verify` checks via the masked avoid-target walk).
#[test]
fn force_any_pairs_generates_puzzles_requiring_the_set() {
    let spec = singles_lc().force_any(mask(PAIRS), 1);
    let easy = singles_lc().baseline_mask(); // singles + LC, with the set removed
    let full = spec.baseline_mask();
    let mut rng = Rng::from_seed(0xF0FACE);
    let mut got = 0;
    for _ in 0..6 {
        let (Some(p), _) = generate(&mut rng, &spec, 20_000) else { continue };
        let grid = p.puzzle.0.clone();
        // Self-consistency: the generator's own cold check accepts what it produced.
        assert!(verify(&grid, &spec), "force_any puzzle rejected by verify: {}", p.puzzle.to_line());
        let board = Board::from_digits(&grid);
        assert!(
            LogicSolver::solve_tracked(&board, full).solved,
            "full baseline must solve: {}",
            p.puzzle.to_line(),
        );
        assert!(
            !LogicSolver::solve_tracked(&board, easy).solved,
            "a pair must be required (singles + LC alone must NOT solve): {}",
            p.puzzle.to_line(),
        );
        got += 1;
    }
    assert!(got > 0, "no force_any(pairs) puzzles produced in cap (test vacuous)");
}

/// Forcing a single member implies the disjunction: a `force(NakedPair)` puzzle satisfies
/// `force_any({NakedPair, HiddenPair})`, and the singleton `force_any({NakedPair})` is the
/// same constraint as `force(NakedPair)` end-to-end.
#[test]
fn forcing_a_member_satisfies_force_any() {
    let force_np = singles_lc().force(NAKED_PAIR, 1);
    let any_pair = singles_lc().force_any(mask(PAIRS), 1);
    let any_np = singles_lc().force_any(mask(&[NAKED_PAIR]), 1);
    let mut rng = Rng::from_seed(0x5EED);
    let mut got = 0;
    for _ in 0..4 {
        let (Some(p), _) = generate(&mut rng, &force_np, 20_000) else { continue };
        let grid = p.puzzle.0.clone();
        assert!(verify(&grid, &force_np), "self-consistency");
        assert!(verify(&grid, &any_pair), "forcing Naked Pair implies the pair disjunction");
        assert!(verify(&grid, &any_np), "singleton force_any must match force end-to-end");
        got += 1;
    }
    assert!(got > 0, "no force(NakedPair) puzzles produced in cap (test vacuous)");
}
