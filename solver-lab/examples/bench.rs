//! Benchmark harness: time each prober variant over a fixed set of strip
//! attempts (the `train(HiddenQuad)` generator's hot loop).
//!
//! Usage: cargo run --release -p solver-lab --example bench -- [--attempts N=2000] [--seed S=1]
//!
//! Every variant in `solvers::REGISTRY` runs the identical seed sequence, so
//! they produce identical puzzles (cross-checked via fingerprint) and differ
//! only in solver time. Reported: total wall time, us/attempt, and a relative
//! speedup vs the baseline (`REGISTRY[0]`). Add a variant by registering it —
//! this harness needs no edit.

use std::time::Instant;

use solver_lab::solvers::REGISTRY;

struct Args {
    attempts: usize,
    seed: u64,
}

fn parse_args() -> Args {
    let mut out = Args { attempts: 2000, seed: 1 };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => out.attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.attempts),
            "--seed" => out.seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.seed),
            "-h" | "--help" => {
                println!("usage: bench [--attempts N] [--seed S]");
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }
    out
}

fn main() {
    let args = parse_args();
    println!(
        "solver-lab bench: {} attempts/variant, seed {} (train(HiddenQuad) strip loop)",
        args.attempts, args.seed
    );
    println!("{} variants registered\n", REGISTRY.len());

    let mut baseline_secs: Option<f64> = None;
    let mut baseline_fp: Option<u64> = None;
    println!("{:<14} {:>10} {:>14} {:>10}  {}", "variant", "secs", "us/attempt", "vs base", "puzzles");
    for v in REGISTRY {
        let start = Instant::now();
        let (fp, givens) = (v.fingerprint)(args.attempts, args.seed);
        let secs = start.elapsed().as_secs_f64();

        match baseline_fp {
            None => baseline_fp = Some(fp),
            Some(b) if b != fp => {
                eprintln!(
                    "FINGERPRINT MISMATCH on `{}` — prover diverged from baseline trajectory!",
                    v.name
                );
                std::process::exit(1);
            }
            _ => {}
        }
        let speedup = match baseline_secs {
            None => {
                baseline_secs = Some(secs);
                1.0
            }
            Some(b) => b / secs,
        };
        let us = secs * 1e6 / args.attempts as f64;
        let avg_givens = givens as f64 / args.attempts as f64;
        println!(
            "{:<14} {:>10.3} {:>14.1} {:>9.2}x  avg {:.1} givens",
            v.name, secs, us, speedup, avg_givens
        );
    }
}
