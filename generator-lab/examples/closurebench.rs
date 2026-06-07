//! Isolated **cheap-closure** microbenchmark: nothing but the scalar
//! [`FusedLogicSolver::solve_tracked`] (singles + LC band_update fixpoint, then the
//! subset ladder) over a faithful corpus of baseline-gate boards. No SIMT solver, no
//! prober, no strip walk, no fill in the measured region — so `perf stat`/`perf record`
//! attribute cleanly to `propagate`/`drain_naked_singles`/`band_update_*`.
//!
//! The corpus is harvested once via [`collect_baseline_boards`] (the exact boards the
//! production baseline gate sees), converted to [`DualSolverState`] up front so only the
//! solve is timed, then replayed `reps` times.
//!
//! Usage: cargo run --release --example closurebench -- [corpus_att=16000] [reps=40] [mode=0|1]
//! (mode 0 = train(HiddenQuad), the with-LC closure; mode 1 = drill(HiddenQuad), no LC.)

use generator_lab::generate::random_simt::collect_baseline_boards;
use generator_lab::repr::Marks;
use generator_lab::repr::banded::DualSolverState;
use generator_lab::solve::{FusedLogicSolver, Solver};
use generator_lab::subset_spec_for_mode;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let corpus_att: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(16000);
    let reps: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);
    let mode: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let spec = subset_spec_for_mode(mode);
    let baseline = spec.baseline_mask();

    let lanes = 8;
    let per_lane = corpus_att / lanes;
    let boards = collect_baseline_boards(1, &spec, lanes, per_lane);
    let duals: Vec<DualSolverState> = boards.iter().map(DualSolverState::from_digits).collect();
    let total = reps as f64 * duals.len() as f64;

    // Warmup + checksum (solved count) so the optimizer can't elide the solve and we can
    // confirm the verdict set is unchanged across variants.
    let mut solved = 0usize;
    for d in &duals {
        if FusedLogicSolver::solve_tracked(d, baseline).solved {
            solved += 1;
        }
    }

    let t = Instant::now();
    let mut sink = 0usize;
    for _ in 0..reps {
        for d in &duals {
            if FusedLogicSolver::solve_tracked(d, baseline).solved {
                sink += 1;
            }
        }
    }
    let dt = t.elapsed();
    std::hint::black_box(sink);
    let ns = dt.as_secs_f64() * 1e9 / total;
    println!(
        "closurebench mode={mode}: corpus {} boards (from {} att), {reps} reps",
        duals.len(),
        per_lane * lanes
    );
    println!("  scalar closure {:>7.2} ns/board   ({:.3} M boards/s)   solved {}", ns, 1e3 / ns, solved);
}
