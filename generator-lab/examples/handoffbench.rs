//! Sizes the fill->strip *handoff* cost (`StripState::new` = `from_digits` + `clue_map`),
//! the serial host work that runs right after the fill in the warp's `start_attempt`.
//!
//! `from_digits` on a COMPLETE grid is provably trivial — every cell is placed, so
//! `unsolved` ends EMPTY and every candidate board ends EMPTY (the 81x2-view peer-clear
//! loop is all no-ops) — i.e. a constant "solved" `DualSolverState`. If it shows up as a
//! meaningful fraction here, `StripState::new` can skip it (construct the solved dual
//! directly) and keep only `clue_map`, byte-identically.
//!
//!   cargo run --release -p generator-lab --example handoffbench -- [n]
use generator_lab::fill::random_solution;
use generator_lab::repr::Marks;
use generator_lab::repr::banded::DualSolverState;
use generator_lab::rng::Rng;
use std::hint::black_box;
use std::time::Instant;

fn main() {
    let n: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2_000_000);

    // Pre-generate the solutions so the fill itself is out of the handoff timings.
    let sols: Vec<_> = (0..n).map(|s| random_solution(&mut Rng::from_seed(s))).collect();

    // warmup
    for s in sols.iter().take((n / 10).max(1) as usize) {
        black_box(DualSolverState::from_digits(&s.0));
        black_box(DualSolverState::clue_map(&s.0));
    }

    for _ in 0..3 {
        let t = Instant::now();
        for i in 0..n {
            black_box(random_solution(&mut Rng::from_seed(i)));
        }
        let fill_us = t.elapsed().as_secs_f64() * 1e6 / n as f64;

        let t = Instant::now();
        for s in &sols {
            black_box(DualSolverState::from_digits(&s.0));
        }
        let fd_us = t.elapsed().as_secs_f64() * 1e6 / n as f64;

        let t = Instant::now();
        for s in &sols {
            black_box(DualSolverState::clue_map(&s.0));
        }
        let cm_us = t.elapsed().as_secs_f64() * 1e6 / n as f64;

        println!(
            "fill {fill_us:>7.3}   from_digits {fd_us:>7.3}   clue_map {cm_us:>7.3}   us each  (from_digits = {:.1}% of fill)",
            100.0 * fd_us / fill_us
        );
    }
}
