//! Native generator benchmark: run a fixed number of strip attempts for
//! train(HiddenQuad) and drill(HiddenQuad), reporting the per-attempt cost and
//! the yield (puzzles found, attempts/puzzle, avg givens, requirement
//! breakdown). Fixed-work and deterministic per seed.
//!
//! Usage: cargo run --release -p generator-lab --example bench -- [--attempts N=4000] [--seed S=1]

use std::time::Instant;

use generator_lab::generator::run_attempts;
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;

struct Args {
    attempts: usize,
    seed: u64,
}

fn parse_args() -> Args {
    let mut out = Args { attempts: 4000, seed: 1 };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => out.attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.attempts),
            "--seed" => out.seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.seed),
            "-h" | "--help" => {
                println!("usage: bench [--attempts N] [--seed S]");
                std::process::exit(0);
            }
            _ => {}
        }
    }
    out
}

fn main() {
    let args = parse_args();
    println!(
        "generator-lab bench: {} attempts/mode, seed {} (random method)\n",
        args.attempts, args.seed
    );
    println!(
        "{:<7} {:>9} {:>13} {:>9} {:>11} {:>11} {:>11} {:>9}",
        "mode", "secs", "us/attempt", "puzzles", "atts/puzzle", "never-fired", "not-forced", "givens"
    );

    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let spec = spec_for_mode(mode);
        let mut rng = Rng::from_seed(args.seed);
        let start = Instant::now();
        let (stats, fp) = run_attempts(&mut rng, &spec, args.attempts);
        let secs = start.elapsed().as_secs_f64();
        let us = secs * 1e6 / args.attempts as f64;
        let atts_per = if stats.successes > 0 {
            stats.attempts as f64 / stats.successes as f64
        } else {
            f64::INFINITY
        };
        let avg_givens = if stats.successes > 0 {
            stats.total_givens as f64 / stats.successes as f64
        } else {
            0.0
        };
        println!(
            "{:<7} {:>9.3} {:>13.1} {:>9} {:>11.1} {:>11} {:>11} {:>9.1}  (fp {:#010x})",
            label,
            secs,
            us,
            stats.successes,
            atts_per,
            stats.never_fired,
            stats.not_forced,
            avg_givens,
            fp as u32,
        );
    }
}
