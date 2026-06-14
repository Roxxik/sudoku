//! Native scalar generator benchmark: run a fixed number of strip attempts for
//! train(HiddenQuad) and drill(HiddenQuad), reporting the per-attempt cost and the
//! yield, carrying the production (full 2-digit) UA pre-filter (docs/UA-FILTER.md).
//! Fixed-work and deterministic per seed.
//!
//! Usage: cargo run --release -p generator-lab --example bench -- [--attempts N=4000] [--seed S=1]

use std::time::Instant;

use generator_lab::generate::{run_attempts, ua_build_cost};
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{HIDDEN_QUAD, NAKED_PAIR};

struct Args {
    attempts: usize,
    seed: u64,
    /// Forced target kind. Defaults to the production `HiddenQuad`; an easier kind
    /// (e.g. `naked-pair`) makes attempts yield puzzles, so the determinism fp folds a
    /// real puzzle stream and bites as an A/B output-identity check (it is constant at
    /// the 0-yield HiddenQuad rarity).
    kind: usize,
}

fn parse_args() -> Args {
    let mut out = Args { attempts: 4000, seed: 1, kind: HIDDEN_QUAD };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => out.attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.attempts),
            "--seed" => out.seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.seed),
            "--kind" => {
                out.kind = match it.next().as_deref() {
                    Some("naked-pair") => NAKED_PAIR,
                    Some("hidden-quad") | None => HIDDEN_QUAD,
                    Some(other) => {
                        eprintln!("unknown --kind {other:?} (naked-pair|hidden-quad)");
                        std::process::exit(2);
                    }
                }
            }
            "-h" | "--help" => {
                println!("usage: bench [--attempts N] [--seed S] [--kind naked-pair|hidden-quad]");
                std::process::exit(0);
            }
            _ => {}
        }
    }
    out
}

fn main() {
    let args = parse_args();
    let kind_label = if args.kind == NAKED_PAIR { "naked-pair" } else { "hidden-quad" };
    println!(
        "generator-lab bench: {} attempts/mode, seed {}, target {} (random method)\n",
        args.attempts, args.seed, kind_label
    );

    // UA pre-filter per-board build cost (isolated enumeration time) for the production full
    // 2-digit library.
    let (nanos, uas) = ua_build_cost(args.seed, args.attempts);
    println!(
        "  UA build full: {:.1} ns/board, {:.1} UAs/board\n",
        nanos as f64 / args.attempts as f64,
        uas as f64 / args.attempts as f64,
    );

    println!(
        "{:<7} {:>9} {:>13} {:>9} {:>11} {:>11} {:>11} {:>9}",
        "mode", "secs", "us/attempt", "puzzles", "atts/puzzle", "never-fired", "not-forced", "givens"
    );

    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let spec = if mode == 0 { Spec::train(args.kind) } else { Spec::drill(args.kind) };
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
