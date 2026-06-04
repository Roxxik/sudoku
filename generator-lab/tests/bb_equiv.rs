//! Direct equivalence: the bitboard baseline engine (`bb::BitBoard::baseline`)
//! must agree with the proven scalar engine (`techniques::solve_tracked`) on
//! every board the strip loop feeds it, for ANY spec shape. The contract the
//! generator actually relies on:
//!   - same `solved` verdict, and
//!   - same `requirement_met` verdict (the only counts the strip loop reads are
//!     the Forced kinds'), and
//!   - identical subset counts (NakedPair..HiddenQuad).
//!
//! The cheap-kind counts (singles, the two locked-candidate orientations) are NOT
//! compared per-kind: the fast path's fused closure reorders them (a cell the
//! scalar resolves as a naked single the closure may resolve as a hidden single;
//! it applies within-band LC eagerly even when singles alone would solve), so
//! their per-kind fired-or-not is not order-invariant. That is sound — the
//! generator never reads a cheap kind's count unless it is Forced, and a Forced
//! cheap kind (or a single-orientation LC, or absent singles) routes to the
//! discrete reference engine, which this test also exercises.
//!
//! Subset counts MUST match exactly because both engines reach the identical
//! {singles, locked-candidates} fixpoint before any subset can fire (the scalar
//! exhausts every easier technique first; the fast baseline drains them in its
//! fused closure), so each subset-decision point sees the same board.
//!
//! Walks REAL strip trajectories so the boards are exactly the post-uniqueness-
//! gate boards the baseline sees in production.

use generator_lab::bb::BitBoard;
use generator_lab::generator::random_full_grid;
use generator_lab::grid::{Board, CELLS, digit_to_bit};
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::spec_for_mode;
use generator_lab::technique_kinds::{
    HIDDEN_PAIR, HIDDEN_SINGLE, LC_CLAIMING, LC_POINTING, NAKED_PAIR, NAKED_SINGLE, NUM,
};
use generator_lab::techniques::solve_tracked;

fn check_spec(label: &str, spec: &Spec, attempts: usize, min_compared: usize) {
    let baseline = spec.baseline_mask();
    let forced = spec.forced_mask();
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

            let bb = BitBoard::from_board(&puzzle);
            let v_bit = digit_to_bit(solution.cell(i));
            let alts = puzzle.candidates(i) & !v_bit;
            if alts != 0 && bb.any_alt_solves(i, alts) {
                puzzle.place(i, orig);
                continue;
            }

            // Both engines on the identical board.
            let scal = solve_tracked(&puzzle, baseline);
            let bbo = bb.baseline(baseline, forced);
            compared += 1;

            assert_eq!(
                scal.solved, bbo.solved,
                "[{label}] solved mismatch on {}",
                puzzle.to_line()
            );
            assert_eq!(
                spec.requirement_met(&scal.counts),
                spec.requirement_met(&bbo.counts),
                "[{label}] requirement_met mismatch (scalar {:?} vs bb {:?}) on {}",
                scal.counts,
                bbo.counts,
                puzzle.to_line()
            );
            for k in NAKED_PAIR..NUM {
                assert_eq!(
                    scal.counts[k], bbo.counts[k],
                    "[{label}] subset kind {k} count mismatch (scalar {} vs bb {}) on {}",
                    scal.counts[k],
                    bbo.counts[k],
                    puzzle.to_line()
                );
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

// --- the two production specs, end-to-end trajectories ------------------------

#[test]
fn bitboard_baseline_matches_scalar_train() {
    check_spec("train", &spec_for_mode(0), 400, 1000);
}

#[test]
fn bitboard_baseline_matches_scalar_drill() {
    check_spec("drill", &spec_for_mode(1), 400, 1000);
}

// --- arbitrary spec shapes: exercise every dispatch branch of `baseline` ------
//
// Each line below names which baseline path it lands in, so a regression in the
// dispatcher (fast vs discrete selection) shows up as a wrong-path mismatch.

const ATT: usize = 150;
const MIN: usize = 300;

/// Singles only, one at a time. Only naked single (no hidden single) -> discrete
/// path (the fast path requires hidden single present).
#[test]
fn spec_naked_single_only() {
    check_spec("ns-only", &Spec::explicit().allow(NAKED_SINGLE), ATT, MIN);
}

/// Only hidden single, no naked single -> discrete path.
#[test]
fn spec_hidden_single_only() {
    check_spec("hs-only", &Spec::explicit().allow(HIDDEN_SINGLE), ATT, MIN);
}

/// Both singles, no LC -> fast path with LC compiled out (`baseline_fast::<false>`).
#[test]
fn spec_both_singles() {
    check_spec(
        "both-singles",
        &Spec::explicit().allow(NAKED_SINGLE).allow(HIDDEN_SINGLE),
        ATT,
        MIN,
    );
}

/// Singles + one Forced LC orientation. A single-orientation LC AND a Forced
/// cheap kind both force the discrete path (the band LC does both orientations
/// together and cannot count one). Pointing and claiming each on their own.
#[test]
fn spec_force_lc_pointing_alone() {
    check_spec(
        "force-lcp",
        &Spec::explicit()
            .allow(NAKED_SINGLE)
            .allow(HIDDEN_SINGLE)
            .force(LC_POINTING, 1),
        ATT,
        MIN,
    );
}

#[test]
fn spec_force_lc_claiming_alone() {
    check_spec(
        "force-lcc",
        &Spec::explicit()
            .allow(NAKED_SINGLE)
            .allow(HIDDEN_SINGLE)
            .force(LC_CLAIMING, 1),
        ATT,
        MIN,
    );
}

/// All four easy kinds allowed (both singles + both LC), Forcing a pair. The
/// Forced kind is a subset, so no cheap kind is Forced -> fast path with full LC
/// (`baseline_fast::<true>`); the Forced pair is counted exactly by the subset
/// ladder.
#[test]
fn spec_allow_four_force_naked_pair() {
    check_spec(
        "all4-force-np",
        &Spec::explicit()
            .allow(NAKED_SINGLE)
            .allow(HIDDEN_SINGLE)
            .allow(LC_POINTING)
            .allow(LC_CLAIMING)
            .force(NAKED_PAIR, 1),
        ATT,
        MIN,
    );
}

#[test]
fn spec_allow_four_force_hidden_pair() {
    check_spec(
        "all4-force-hp",
        &Spec::explicit()
            .allow(NAKED_SINGLE)
            .allow(HIDDEN_SINGLE)
            .allow(LC_POINTING)
            .allow(LC_CLAIMING)
            .force(HIDDEN_PAIR, 1),
        ATT,
        MIN,
    );
}

// A spec that allows ONLY one single and concedes the other plus LC is confluent
// (a lone single technique has nothing that degenerates), so it is supported. We
// pin it against core directly with two fixtures the authoritative CLI generated
// and verified (`cargo run -p sudoku-cli -- --allow <single> --require <single>=1
// --concede <other-single>,pointing,claiming`):
//
//   - HS_ONLY solves with hidden single alone, STUCK with naked single alone.
//   - NS_ONLY solves with naked single alone, STUCK with hidden single alone.
//
// The combinations the discrete path could mishandle — a single MISSING while LC
// is present — are deliberately absent: those are non-confluent (an LC elimination
// can leave a naked or hidden single the toolbox cannot place, so the verdict
// depends on scan order) and are NOT supported (the `baseline` debug_assert trips
// on them). Hence no `allow-one-single + concede-other + force-LC` cases here.

/// Hidden-single-only baseline (naked single + LC conceded). Solvable by hidden
/// singles alone per core.
const HS_ONLY: &str = "37.6.......5.2..6.4....13..1..2....5839...7........8.......21.....53..8..8....23.";
/// Naked-single-only baseline (hidden single + LC conceded). Solvable by naked
/// singles alone per core.
const NS_ONLY: &str = "..86....76..52.8...5..9.63...78..9..23.........4.....55............1..6...13..7.4";

/// Solve `line` with `spec`'s baseline via BOTH engines and assert they agree
/// with each other and with core's ground-truth `expect_solved`. When solvable,
/// the Forced single must have fired (requirement met) in both.
fn check_fixture(label: &str, line: &str, spec: &Spec, expect_solved: bool) {
    let board = Board::parse(line).expect("valid puzzle line");
    let baseline = spec.baseline_mask();
    let forced = spec.forced_mask();
    let scal = solve_tracked(&board, baseline);
    let bb = BitBoard::from_board(&board);
    let bbo = bb.baseline(baseline, forced);
    assert_eq!(scal.solved, expect_solved, "[{label}] scalar solved vs core");
    assert_eq!(bbo.solved, expect_solved, "[{label}] bb solved vs core");
    assert_eq!(
        spec.requirement_met(&scal.counts),
        spec.requirement_met(&bbo.counts),
        "[{label}] requirement_met disagreement (scalar {:?} vs bb {:?})",
        scal.counts,
        bbo.counts,
    );
}

/// Baseline = hidden single alone, naked single + LC conceded, force hidden single.
#[test]
fn fixture_hidden_single_only_baseline() {
    let hs = Spec::explicit()
        .force(HIDDEN_SINGLE, 1)
        .concede(NAKED_SINGLE)
        .concede(LC_POINTING)
        .concede(LC_CLAIMING);
    check_fixture("hs-only/hs-baseline", HS_ONLY, &hs, true);
    // Mirror: naked-single-only baseline cannot crack it (core: STUCK).
    let ns = Spec::explicit().force(NAKED_SINGLE, 1);
    check_fixture("hs-only/ns-baseline", HS_ONLY, &ns, false);
}

/// Baseline = naked single alone, hidden single + LC conceded, force naked single.
#[test]
fn fixture_naked_single_only_baseline() {
    let ns = Spec::explicit()
        .force(NAKED_SINGLE, 1)
        .concede(HIDDEN_SINGLE)
        .concede(LC_POINTING)
        .concede(LC_CLAIMING);
    check_fixture("ns-only/ns-baseline", NS_ONLY, &ns, true);
    // Mirror: hidden-single-only baseline cannot crack it (core: STUCK).
    let hs = Spec::explicit().force(HIDDEN_SINGLE, 1);
    check_fixture("ns-only/hs-baseline", NS_ONLY, &hs, false);
}
