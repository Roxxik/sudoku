//! Phase-split profiler: where does generation time actually go? Times the four
//! phases of the per-attempt trajectory separately, for train and drill, so the
//! bottleneck is measured (not guessed) before any optimization.
//!
//! Phases:
//!   - grid     : random full grid + position shuffle + initial bb
//!   - bb-maint : apply_clear (reopen cell i, derived from solution + present)
//!   - prober   : uniqueness gate (any_alt_solves)
//!   - baseline : the strip gate-b tracked solve (solvability + requirement counts)
//!   - verify   : irreplaceability check (only runs when a `best` exists)
//!   - other    : place reverts / `present` bookkeeping (the remainder)
//!
//! Usage: cargo run --release -p generator-lab --example profile -- [--attempts N=4000] [--seed S=1]

use std::time::{Duration, Instant};

use generator_lab::bb::{BitBoard, Placed};
use generator_lab::generator::{board_from_cells, random_full_grid};
use generator_lab::grid::{CELLS, Digit, digit_to_bit};
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;
use generator_lab::verify::verify;

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
    let spec = spec_for_mode(mode);
    let baseline = spec.baseline_mask();
    let mut rng = Rng::from_seed(seed);
    let mut p = Phases::default();
    let run = Instant::now();

    for _ in 0..attempts {
        let t = Instant::now();
        let solution = random_full_grid(&mut rng);
        let mut positions: Vec<usize> = (0..CELLS).collect();
        rng.shuffle(&mut positions);
        let mut bb = BitBoard::from_board(&solution);
        let mut placed = Placed::from_board(&solution);
        let mut cells: [Digit; CELLS] = core::array::from_fn(|i| solution.cell(i));
        p.grid += t.elapsed();

        let mut best: Option<[Digit; CELLS]> = None;
        let mut req_met = false;
        for i in positions {
            if cells[i] == 0 {
                continue;
            }
            let orig = cells[i];
            cells[i] = 0;

            let tbuild = Instant::now();
            let cand = bb.apply_clear(i, orig, &mut placed);
            p.build += tbuild.elapsed();

            let v_bit = digit_to_bit(orig);
            let alts = cand & !v_bit;

            // Fast path: `i` still a naked single -> strip always valid, both
            // gates skippable, requirement verdict carried (mirrors `attempt`).
            if alts == 0 {
                if req_met {
                    best = Some(cells);
                }
                continue;
            }

            let tp = Instant::now();
            let non_unique = bb.any_alt_solves(i, alts);
            p.prober += tp.elapsed();
            if non_unique {
                cells[i] = orig;
                bb.apply_place(i, orig, &mut placed);
                continue;
            }

            let tb = Instant::now();
            let outcome = bb.baseline(baseline);
            p.baseline += tb.elapsed();
            if !outcome.solved {
                cells[i] = orig;
                bb.apply_place(i, orig, &mut placed);
                continue;
            }
            req_met = spec.requirement_met(&outcome.counts);
            if req_met {
                best = Some(cells);
            }
        }

        if let Some(snap) = best {
            let seed_board = board_from_cells(&snap);
            let tv = Instant::now();
            let ok = verify(&seed_board, &spec);
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
        println!("  bb-maint {:>8.1} us/att  {:>5.1}%", us(p.build), pct(p.build));
        println!("  prober   {:>8.1} us/att  {:>5.1}%", us(p.prober), pct(p.prober));
        println!("  baseline {:>8.1} us/att  {:>5.1}%", us(p.baseline), pct(p.baseline));
        println!("  verify   {:>8.1} us/att  {:>5.1}%", us(p.verify), pct(p.verify));
        println!("  other    {:>8.1} us/att  {:>5.1}%", us(other), pct(other));
        println!("  total    {:>8.1} us/att\n", us(p.total));
    }
}
