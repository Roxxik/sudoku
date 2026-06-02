//! Faithfulness cross-check against `core`: the ONLY thing generator-lab must
//! preserve through any refactor is **spec satisfiability** — that the puzzles
//! it accepts are exactly the puzzles core's verifier accepts, for the same
//! spec. Representations and code structure are free to change; this test is the
//! invariant that guards it.
//!
//! Two directions, on fast targets (so the test stays quick — the spec machinery
//! exercised is identical regardless of target):
//!   - glab generates  -> core::verify accepts.
//!   - core generates   -> glab::verify accepts.
//!
//! HiddenQuad itself is checked behind `--ignored` (mean ~74k attempts/puzzle is
//! too slow for the default run).

use generator_lab::generator::generate as glab_generate;
use generator_lab::grid::Board as GBoard;
use generator_lab::rng::Rng as GRng;
use generator_lab::spec::Spec as GSpec;
use generator_lab::techniques as gt;
use generator_lab::verify::verify as glab_verify;

use sudoku_core::{
    Board as CBoard, Rng as CRng, Spec as CSpec, TechniqueKind as CK, make_puzzle_for_spec, verify as core_verify,
};

/// glab kind index -> core TechniqueKind (PoC ladder).
fn core_kind(idx: usize) -> CK {
    match idx {
        gt::NAKED_SINGLE => CK::NakedSingle,
        gt::HIDDEN_SINGLE => CK::HiddenSingle,
        gt::LC_POINTING => CK::LockedCandidatesPointing,
        gt::LC_CLAIMING => CK::LockedCandidatesClaiming,
        gt::NAKED_PAIR => CK::NakedPair,
        gt::HIDDEN_PAIR => CK::HiddenPair,
        gt::NAKED_TRIPLE => CK::NakedTriple,
        gt::HIDDEN_TRIPLE => CK::HiddenTriple,
        gt::NAKED_QUAD => CK::NakedQuad,
        gt::HIDDEN_QUAD => CK::HiddenQuad,
        _ => unreachable!(),
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Train,
    Drill,
}

fn glab_spec(mode: Mode, target: usize) -> GSpec {
    match mode {
        Mode::Train => GSpec::train(target),
        Mode::Drill => GSpec::drill(target),
    }
}

fn core_spec(mode: Mode, target: usize) -> CSpec {
    let k = core_kind(target);
    match mode {
        Mode::Train => CSpec::train(k),
        Mode::Drill => CSpec::drill(k),
    }
}

/// glab generates `want` puzzles; each must pass core::verify (and glab::verify,
/// a self-consistency sanity).
fn glab_generated_pass_core(mode: Mode, target: usize, want: usize, per_cap: usize) {
    let gspec = glab_spec(mode, target);
    let cspec = core_spec(mode, target);
    let mut rng = GRng::from_seed(0xA11CE);
    let mut got = 0;
    for _ in 0..want {
        let (puzzle, _stats) = glab_generate(&mut rng, &gspec, per_cap);
        let Some(p) = puzzle else { continue };
        let line = p.puzzle.to_line();

        // glab self-consistency.
        let gb = GBoard::parse(&line).expect("glab parse");
        assert!(glab_verify(&gb, &gspec), "glab rejected its own puzzle: {line}");

        // The real check: core's verifier accepts it for the equivalent spec.
        let cb = CBoard::parse(&line).expect("core parse");
        assert!(
            core_verify(&cb, &cspec).is_ok(),
            "core REJECTED a glab puzzle (target {}, line {line}): {:?}",
            gt::NAMES[target],
            core_verify(&cb, &cspec).err(),
        );
        got += 1;
    }
    assert!(got > 0, "glab produced no {} puzzles in cap (test vacuous)", gt::NAMES[target]);
}

/// core generates `want` puzzles; each must pass glab::verify.
fn core_generated_pass_glab(mode: Mode, target: usize, want: usize, per_cap: usize) {
    let gspec = glab_spec(mode, target);
    let cspec = core_spec(mode, target);
    let mut rng = CRng::from_seed(0xB0B);
    let mut got = 0;
    for _ in 0..want {
        let Some(res) = make_puzzle_for_spec(&mut rng, &cspec, per_cap) else { continue };
        let line = res.puzzle.puzzle.to_line();
        let gb = GBoard::parse(&line).expect("glab parse");
        assert!(
            glab_verify(&gb, &gspec),
            "glab REJECTED a core puzzle (target {}, line {line})",
            gt::NAMES[target],
        );
        got += 1;
    }
    assert!(got > 0, "core produced no {} puzzles in cap (test vacuous)", gt::NAMES[target]);
}

#[test]
fn train_naked_pair_both_directions() {
    glab_generated_pass_core(Mode::Train, gt::NAKED_PAIR, 4, 20_000);
    core_generated_pass_glab(Mode::Train, gt::NAKED_PAIR, 4, 20_000);
}

#[test]
fn drill_naked_pair_both_directions() {
    glab_generated_pass_core(Mode::Drill, gt::NAKED_PAIR, 4, 60_000);
    core_generated_pass_glab(Mode::Drill, gt::NAKED_PAIR, 4, 60_000);
}

#[test]
fn train_hidden_pair_both_directions() {
    glab_generated_pass_core(Mode::Train, gt::HIDDEN_PAIR, 4, 40_000);
    core_generated_pass_glab(Mode::Train, gt::HIDDEN_PAIR, 4, 40_000);
}

/// HiddenQuad is the real target but ~74k attempts/puzzle — run on demand:
/// `cargo test -p generator-lab --release -- --ignored`.
#[test]
#[ignore]
fn train_hidden_quad_both_directions() {
    glab_generated_pass_core(Mode::Train, gt::HIDDEN_QUAD, 1, 2_000_000);
    core_generated_pass_glab(Mode::Train, gt::HIDDEN_QUAD, 1, 2_000_000);
}

/// Cross-backend + regression anchor for the RNG/fill stream. glab's bounded RNG
/// (`range`) uses Lemire multiply-shift, so the stream intentionally DIVERGES
/// from core's — faithfulness (the tests above) is preserved because it only
/// requires verify-agreement, not an identical stream. This pins the
/// grid-sensitive `determinism_fp` (solution grid + strip order of every
/// attempt) per seed: the wasm `det_fp` export MUST return the same u32 truncation
/// on-device, and any unintended change to the shuffle/fill trips this. If you
/// change the RNG or fill on purpose, re-baseline these values.
#[test]
fn determinism_fp_pinned() {
    use generator_lab::generator::determinism_fp;
    for (seed, want) in [
        (1u64, 0xeff10e346cad1dc9u64),
        (2, 0xf6ea34a6fbad2d0d),
        (3, 0x344ccb182c947cad),
    ] {
        let mut rng = GRng::from_seed(seed);
        assert_eq!(
            determinism_fp(&mut rng, 2000),
            want,
            "determinism_fp changed for seed {seed} — re-baseline if the RNG/fill change was intentional, \
             and update the wasm det_fp guard"
        );
    }
}
