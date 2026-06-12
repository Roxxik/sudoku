//! The strip loop's keep/required decisions are fixpoint properties, so reordering the
//! solver's harder ladder must not change the solve fixpoint. This guards that: it
//! generates a few puzzles for representative specs and asserts every permutation of the
//! subset/fish/wing block reaches the byte-identical fixpoint (placements + candidates)
//! the production order does — on the full toolbox and on the necessity toolbox (minus
//! the forced kind). The exhaustive sweep lives in `examples/confluence.rs`.
//!
//! Release mode (it does real SIMT generation): `cargo test --release -p generator-lab`.

use generator_lab::expert_spec;
use generator_lab::generate::warp_host::{GateStream, Pumped};
use generator_lab::repr::Board;
use generator_lab::solve::{
    HARD_STEPS_DEFAULT, HardStep, LogicSolver, Solver, solve_fixpoint_with_order,
};
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{NAKED_TRIPLE, SWORDFISH, W_WING, X_WING, XY_WING};

/// FNV fold of a fixpoint board: per cell, placed digit (0 = empty) in the high bits and
/// the 9-bit candidate mask in the low — equal iff every cell agrees on both.
fn fixpoint_fp(b: &Board) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for c in 0..81 {
        let placed = b.cell(c).map(|d| d.index() as u16 + 1).unwrap_or(0);
        let code = (placed << 9) | b.marks(c).bits();
        h ^= code as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn permutations() -> Vec<Vec<HardStep>> {
    use HardStep::*;
    vec![
        HARD_STEPS_DEFAULT.iter().rev().copied().collect(),
        vec![Wings, NakedSubset(2), HiddenSubset(2), NakedSubset(3), HiddenSubset(3), NakedSubset(4), HiddenSubset(4), Fish],
        vec![Fish, Wings, NakedSubset(2), HiddenSubset(2), NakedSubset(3), HiddenSubset(3), NakedSubset(4), HiddenSubset(4)],
    ]
}

/// Generate `count` puzzles for `spec`, then assert the harder-ladder reorderings all
/// reach the production-order fixpoint on the full toolbox and on full-minus-forced.
fn assert_confluent(spec: &Spec, count: u64) {
    let full = spec.baseline_mask();
    let toolboxes = [full, full & !spec.forced_mask()];
    let perms = permutations();

    let mut boards: Vec<(u64, Board)> = Vec::new();
    // One seed = one attempt, so feed an unbounded seed range and stop once `count` puzzles
    // have surfaced. A fixed `1..1+count` range would yield far fewer than `count` for a
    // rare spec (Swordfish/W-Wing need many attempts each).
    let mut stream = GateStream::new(1.., spec);
    while (boards.len() as u64) < count {
        match stream.pump(4096) {
            Pumped::Found(seed, p) => boards.push((seed, Board::from_digits(&p.puzzle.0))),
            Pumped::StepCountReached => {}
            Pumped::NoMorePuzzles => break,
        }
    }
    assert!(!boards.is_empty(), "spec produced no puzzles to test");

    for (seed, board) in &boards {
        for &mask in &toolboxes {
            let reference = solve_fixpoint_with_order::<Board>(board, mask, &HARD_STEPS_DEFAULT);
            // The default-order reference must agree with the production solver's verdict.
            assert_eq!(
                reference.is_complete(),
                LogicSolver::solve_tracked(board, mask).solved,
                "default-order harness disagrees with LogicSolver (seed {seed})"
            );
            let want = fixpoint_fp(&reference);
            for perm in &perms {
                let got = fixpoint_fp(&solve_fixpoint_with_order::<Board>(board, mask, perm));
                assert_eq!(
                    got, want,
                    "harder-ladder reorder changed the fixpoint (seed {seed}, mask {mask:#x}): {got:#018x} != {want:#018x}\n  {}",
                    board.digits().to_line()
                );
            }
        }
    }
}

#[test]
fn reorder_preserves_fixpoint_subset_and_fish() {
    assert_confluent(&expert_spec(NAKED_TRIPLE, false), 8);
    assert_confluent(&expert_spec(X_WING, false), 8);
    assert_confluent(&expert_spec(SWORDFISH, false), 6);
}

#[test]
fn reorder_preserves_fixpoint_wings() {
    assert_confluent(&expert_spec(XY_WING, false), 8);
    assert_confluent(&expert_spec(W_WING, false), 6);
}
