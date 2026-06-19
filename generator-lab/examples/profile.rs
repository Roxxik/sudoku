//! Phase-split profiler: where does generation time actually go? Times the phases
//! of the per-attempt trajectory separately, for train and drill, so the bottleneck
//! is measured (not guessed) before any optimization. Runs the new-repr strip:
//! incremental dual-banded candidate maintenance, the `Search<Bivalue>` uniqueness
//! prober, and the `FusedLogicSolver` baseline gate.
//!
//! Phases:
//!   - grid     : random solution + position shuffle + initial dual board / clue map
//!   - maint    : clear_clue (reopen cell i in both views, derived from clue map)
//!   - prober   : uniqueness gate (Search<Bivalue> existence query)
//!   - baseline : the strip gate's tracked solve (solvability + requirement counts)
//!   - verify   : irreplaceability check (only runs when a `best` exists)
//!   - other    : place reverts / bookkeeping (the remainder)
//!
//! Usage: cargo run --release -p generator-lab --example profile -- [--attempts N=4000] [--seed S=1]

use std::time::{Duration, Instant};

use generator_lab::fill::random_solution;
use generator_lab::generate::verify;
use generator_lab::probe::{Prober, Search};
use generator_lab::repr::banded::DualSolverState;
use generator_lab::repr::{CELLS, DigitGrid, Marks};
use generator_lab::rng::Rng;
use generator_lab::scan::Bivalue;
use generator_lab::solve::{FusedLogicSolver, Solver};
use generator_lab::subset_spec_for_mode;

/// The uniqueness prober: scan/sieve `Search` with the `Bivalue` branch strategy.
type P = Search<Bivalue>;

#[derive(Default)]
struct Phases {
    grid: Duration,
    build: Duration,
    prober: Duration,
    baseline: Duration,
    verify: Duration,
    total: Duration,
    successes: usize,
    verifies: usize,
}

fn profile(mode: u32, attempts: usize, seed: u64) -> Phases {
    let spec = subset_spec_for_mode(mode);
    let baseline = spec.baseline_mask();
    let mut rng = Rng::from_seed(seed);
    let mut p = Phases::default();
    let run = Instant::now();

    for _ in 0..attempts {
        let t = Instant::now();
        let solution = random_solution(&mut rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        let mut digits: DigitGrid = solution.0.clone();
        let mut dual = DualSolverState::from_digits(&digits);
        let mut clue = DualSolverState::clue_map(&digits);
        p.grid += t.elapsed();

        let mut best: Option<DigitGrid> = None;
        let mut req_met = false;
        for i in positions {
            let Some(orig) = digits.get(i) else {
                continue;
            };
            digits.clear(i);

            let tbuild = Instant::now();
            let cand = dual.clear_clue(&mut clue, i, orig);
            p.build += tbuild.elapsed();

            // Fast path: `i` still a naked single -> strip always valid, both
            // gates skippable, requirement verdict carried (mirrors `attempt`).
            if cand.len() == 1 {
                if req_met {
                    best = Some(digits.clone());
                }
                continue;
            }

            let tp = Instant::now();
            let mut probe = dual.row().clone();
            probe.forbid(i, orig);
            let non_unique = P::has_completion(probe);
            p.prober += tp.elapsed();
            if non_unique {
                digits.set(i, orig);
                dual.place_clue(&mut clue, i, orig);
                continue;
            }

            let tb = Instant::now();
            let outcome = FusedLogicSolver::solve_tracked(dual.row(), baseline);
            p.baseline += tb.elapsed();
            if !outcome.solved {
                digits.set(i, orig);
                dual.place_clue(&mut clue, i, orig);
                continue;
            }
            req_met = spec.requirement_met(&outcome.counts);
            if req_met {
                best = Some(digits.clone());
            }
        }

        if let Some(snap) = best {
            let tv = Instant::now();
            let ok = verify(&snap, &spec);
            p.verify += tv.elapsed();
            p.verifies += 1;
            if ok {
                p.successes += 1;
            }
        }
    }
    p.total = run.elapsed();
    p
}

fn main() {
    let mut attempts = 4000usize;
    let mut seed = 1u64;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
            "--seed" => seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(seed),
            _ => {}
        }
    }

    println!("generator-lab profile: {attempts} attempts/mode, seed {seed}\n");
    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let p = profile(mode, attempts, seed);
        let us = |d: Duration| d.as_secs_f64() * 1e6 / attempts as f64;
        let pct = |d: Duration| 100.0 * d.as_secs_f64() / p.total.as_secs_f64();
        let other = p.total.saturating_sub(p.grid + p.build + p.prober + p.baseline + p.verify);
        println!("== {label} ==  ({} verifies, {} puzzles)", p.verifies, p.successes);
        println!("  grid     {:>8.1} us/att  {:>5.1}%", us(p.grid), pct(p.grid));
        println!("  maint    {:>8.1} us/att  {:>5.1}%", us(p.build), pct(p.build));
        println!("  prober   {:>8.1} us/att  {:>5.1}%", us(p.prober), pct(p.prober));
        println!("  baseline {:>8.1} us/att  {:>5.1}%", us(p.baseline), pct(p.baseline));
        println!("  verify   {:>8.1} us/att  {:>5.1}%", us(p.verify), pct(p.verify));
        println!("  other    {:>8.1} us/att  {:>5.1}%", us(other), pct(other));
        println!("  total    {:>8.1} us/att\n", us(p.total));
    }
}
