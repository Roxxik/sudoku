//! Where does strip_attempt's time actually go: the uniqueness prober, or the
//! baseline gate? Times the two regions separately on the REAL strip trajectory
//! (batch prober), plus the grid-gen/strip overhead.
//!
//! Also reports baseline cost split by whether the position is unique (baseline
//! runs) vs non-unique (prober rejects first, baseline skipped) — to show why the
//! prober is a cheap pre-filter that prevents the expensive exhaustive-baseline
//! on doomed boards.

use std::time::{Duration, Instant};

use solver_lab::generate::random_full_grid;
use solver_lab::grid::{CELLS, digit_to_bit};
use solver_lab::rng::Rng;
use solver_lab::solvers::UniqProber;
use solver_lab::solvers::batch;
use solver_lab::techniques::baseline_solvable;

fn main() {
    let attempts = 4000usize;
    let seed = 1u64;
    let mut rng = Rng::from_seed(seed);

    let mut t_prober = Duration::ZERO;
    let mut t_baseline = Duration::ZERO;
    let mut t_overhead = Duration::ZERO;
    let mut sink = 0u64;

    for _ in 0..attempts {
        let o = Instant::now();
        let solution = random_full_grid(&mut rng);
        let mut puzzle = solution.clone();
        let mut positions: Vec<usize> = (0..CELLS).collect();
        rng.shuffle(&mut positions);
        t_overhead += o.elapsed();

        for i in positions {
            if puzzle.is_empty(i) {
                continue;
            }
            let orig = puzzle.cell(i);
            puzzle.clear_naked(i);

            let v_bit = digit_to_bit(solution.cell(i));
            let alts = puzzle.candidates(i) & !v_bit;

            let p = Instant::now();
            let mut non_unique = false;
            if alts != 0 {
                let mut probe = batch::Probe::from_board(&puzzle);
                non_unique = probe.any_alt_solves(i, alts);
            }
            t_prober += p.elapsed();

            if non_unique {
                puzzle.place(i, orig);
                continue;
            }

            let b = Instant::now();
            let solvable = baseline_solvable(&puzzle);
            t_baseline += b.elapsed();
            sink ^= solvable as u64;

            if !solvable {
                puzzle.place(i, orig);
                continue;
            }
        }
    }

    let us = |d: Duration| d.as_secs_f64() * 1e6 / attempts as f64;
    let total = t_prober + t_baseline + t_overhead;
    println!("strip_attempt split (batch prober), {attempts} attempts, seed {seed}\n");
    println!("  prober (uniq gate, all positions) : {:>8.1} us/att  {:>5.1}%", us(t_prober), 100.0 * t_prober.as_secs_f64() / total.as_secs_f64());
    println!("  baseline (only on unique positions): {:>8.1} us/att  {:>5.1}%", us(t_baseline), 100.0 * t_baseline.as_secs_f64() / total.as_secs_f64());
    println!("  overhead (grid-gen + shuffle)     : {:>8.1} us/att  {:>5.1}%", us(t_overhead), 100.0 * t_overhead.as_secs_f64() / total.as_secs_f64());
    println!("  total                              : {:>8.1} us/att  (sink {sink})", us(total));
}
