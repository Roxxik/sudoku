use std::time::Instant;
use sudoku_core::{REGISTRY, Rng, TechniqueKind, make_puzzle, make_puzzle_forced, make_puzzle_needing};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(|s| s.as_str()).unwrap_or("basket");
    match mode {
        "basket" => bench_basket(),
        "one" => {
            let target_name = args.get(1).map(|s| s.as_str()).unwrap_or("swordfish");
            let seeds: Vec<u64> = args
                .get(2)
                .map(|s| s.split(',').filter_map(|x| x.parse().ok()).collect())
                .unwrap_or_else(|| vec![1, 2, 3]);
            let forced = args.get(3).map(|s| s.as_str()) != Some("needs");
            let target = lookup(&target_name);
            bench(target, &seeds, forced);
        }
        other => {
            eprintln!("unknown mode {:?}", other);
            std::process::exit(2);
        }
    }
}

fn lookup(name: &str) -> TechniqueKind {
    REGISTRY
        .iter()
        .find(|d| d.cli_name == name)
        .map(|d| d.kind)
        .unwrap_or_else(|| {
            eprintln!("unknown technique {:?}", name);
            std::process::exit(2);
        })
}

fn bench_basket() {
    // A basket covering distinct code paths. Same seeds every run so
    // attempt counts are identical and timings are directly comparable.
    let runs: &[(&str, bool, &[u64])] = &[
        ("swordfish", true, &[1, 2]),
        ("x-wing", true, &[1]),
        ("hidden-pair", true, &[1]),
        ("naked-pair", true, &[1]),
        ("xy-wing", true, &[1]),
    ];
    let mut grand_total = 0.0f64;
    let mut grand_attempts = 0usize;
    for &(name, forced, seeds) in runs {
        let target = lookup(name);
        let (dt, atts) = bench(target, seeds, forced);
        grand_total += dt;
        grand_attempts += atts;
    }
    // Easy paths.
    let (dt_full, dt_uniq) = bench_make_puzzle();
    grand_total += dt_full + dt_uniq;
    println!(
        "BASKET TOTAL: {:.3} s, forced-attempts {}, per-forced-attempt {:.3} ms, mk-full {:.2} ms, mk-uniq {:.2} ms",
        grand_total,
        grand_attempts,
        (grand_total - dt_full - dt_uniq) / grand_attempts as f64 * 1000.0,
        dt_full * 1000.0,
        dt_uniq * 1000.0,
    );
}

fn bench_make_puzzle() -> (f64, f64) {
    let seeds = 1u64..=50;
    let t0 = Instant::now();
    let mut sink = 0usize;
    for seed in seeds.clone() {
        let mut rng = Rng::from_seed(seed);
        sink ^= make_puzzle(&mut rng, true).givens;
    }
    let dt_full = t0.elapsed().as_secs_f64();
    println!(
        "--- make_puzzle(true) x{}: {:.3} s ({:.2} ms/iter), sink {} ---",
        seeds.clone().count(),
        dt_full,
        dt_full * 1000.0 / seeds.clone().count() as f64,
        sink,
    );

    let t0 = Instant::now();
    let mut sink = 0usize;
    for seed in seeds.clone() {
        let mut rng = Rng::from_seed(seed);
        sink ^= make_puzzle(&mut rng, false).givens;
    }
    let dt_uniq = t0.elapsed().as_secs_f64();
    println!(
        "--- make_puzzle(false) x{}: {:.3} s ({:.2} ms/iter), sink {} ---",
        seeds.clone().count(),
        dt_uniq,
        dt_uniq * 1000.0 / seeds.clone().count() as f64,
        sink,
    );
    (dt_full, dt_uniq)
}

fn bench(target: TechniqueKind, seeds: &[u64], forced: bool) -> (f64, usize) {
    let mut total = 0.0f64;
    let mut total_attempts = 0usize;
    let label = if forced { "forced" } else { "needs" };
    println!("--- {} {} seeds={:?} ---", label, target.cli_name(), seeds);
    for &seed in seeds {
        let mut rng = Rng::from_seed(seed);
        let t0 = Instant::now();
        let res = if forced {
            make_puzzle_forced(&mut rng, target, 100_000)
        } else {
            make_puzzle_needing(&mut rng, target, 100_000)
        };
        let dt = t0.elapsed().as_secs_f64();
        total += dt;
        match res {
            Some(fr) => {
                total_attempts += fr.attempts;
                println!(
                    "  seed {:>3}: {:>8.2} ms, attempts {:>5}, givens {}",
                    seed,
                    dt * 1000.0,
                    fr.attempts,
                    fr.puzzle.givens,
                );
            }
            None => {
                println!("  seed {}: FAILED in {:.2} ms", seed, dt * 1000.0);
            }
        }
    }
    println!(
        "  TOTAL {:.3} s, attempts {}, per-attempt {:.3} ms",
        total,
        total_attempts,
        total / total_attempts.max(1) as f64 * 1000.0,
    );
    (total, total_attempts)
}
