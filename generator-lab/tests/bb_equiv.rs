//! Direct equivalence: the bitboard baseline engine (`bb::BitBoard::baseline`)
//! must agree with the proven scalar engine (`techniques::solve_tracked`) on
//! every board the strip loop feeds it — same `solved`, and the same set of
//! techniques *fired* (counts > 0). Exact counts may differ (the bitboard engine
//! drains singles in batches), but the strip trajectory only depends on `solved`
//! and "did the required technique fire", both of which this pins.
//!
//! Walks the REAL train and drill trajectories so the boards are exactly the
//! post-uniqueness-gate boards the baseline sees in production.

use generator_lab::bb::BitBoard;
use generator_lab::generator::random_full_grid;
use generator_lab::grid::{CELLS, digit_to_bit};
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;
use generator_lab::techniques::{NUM, solve_tracked};

fn check_mode(mode: u32, attempts: usize) {
    let spec = spec_for_mode(mode);
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

            let bb = BitBoard::from_board(&puzzle);
            let v_bit = digit_to_bit(solution.cell(i));
            let alts = puzzle.candidates(i) & !v_bit;
            if alts != 0 && bb.any_alt_solves(i, alts) {
                puzzle.place(i, orig);
                continue;
            }

            // Both engines on the identical board.
            let scal = solve_tracked(&puzzle, baseline);
            let bbo = bb.baseline(baseline);
            compared += 1;

            assert_eq!(
                scal.solved, bbo.solved,
                "solved mismatch on {}",
                puzzle.to_line()
            );
            for k in 0..NUM {
                assert_eq!(
                    scal.counts[k] > 0,
                    bbo.counts[k] > 0,
                    "kind {k} fired-mismatch (scalar {} vs bb {}) on {}",
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
    assert!(compared > 1000, "too few comparisons ({compared}) — test too weak");
    eprintln!("mode {mode}: {compared} board comparisons agreed");
}

#[test]
fn bitboard_baseline_matches_scalar_train() {
    check_mode(0, 400);
}

#[test]
fn bitboard_baseline_matches_scalar_drill() {
    check_mode(1, 400);
}
