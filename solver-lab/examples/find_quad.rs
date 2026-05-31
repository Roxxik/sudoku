//! Find one actual `train(HiddenQuad)` puzzle and print its 81-char line, so it
//! can be fed to `cli --puzzle <line>` to confirm the trace uses a hidden quad.
//!
//! This is the **real-deal** workload (run the generator until a hidden-quad
//! puzzle drops out), as opposed to the `bench` example's fixed-count
//! microbenchmark. It is registry-driven: pick a prober with `--solver <name>`,
//! pass several (`--solver a,b` or `--solver a --solver b`), or `--solver all`
//! to time every registered prober on the identical search. Because all correct
//! provers walk the identical strip trajectory, every solver finds the quad at
//! the *same* attempt number — so the reported us/attempt is a clean
//! apples-to-apples comparison, both between solvers and against `bench`.
//!
//! Usage: cargo run --release -p solver-lab --example find_quad -- \
//!            [--seed S=1] [--solver NAME=simd-dense | a,b | all]...

use std::time::Instant;

use solver_lab::grid::Board;
use solver_lab::rng::Rng;
use solver_lab::solvers::{REGISTRY, Variant};

const ATTEMPT_CAP: usize = 2_000_000;

struct Args {
    seed: u64,
    solvers: Vec<String>,
}

fn parse_args() -> Args {
    let mut out = Args {
        seed: 1,
        solvers: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--seed" => out.seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.seed),
            "--solver" => {
                if let Some(val) = it.next() {
                    out.solvers.extend(
                        val.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                    );
                }
            }
            "-h" | "--help" => {
                println!("usage: find_quad [--seed S] [--solver NAME|a,b|all]...");
                println!("solvers: all, {}", names().join(", "));
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }
    // Default to the baseline so a bare run reports the reference number.
    if out.solvers.is_empty() {
        out.solvers.push("simd-dense".to_string());
    }
    out
}

fn names() -> Vec<&'static str> {
    REGISTRY.iter().map(|v| v.name).collect()
}

/// Run `v` until it finds a hidden-quad puzzle (or hits the cap). Returns
/// `(attempts, elapsed_secs, puzzle)`.
fn run(v: &Variant, seed: u64) -> (usize, f64, Option<Board>) {
    let mut rng = Rng::from_seed(seed);
    let start = Instant::now();
    for attempt in 1..=ATTEMPT_CAP {
        if let Some(puzzle) = (v.quad_attempt)(&mut rng) {
            return (attempt, start.elapsed().as_secs_f64(), Some(puzzle));
        }
    }
    (ATTEMPT_CAP, start.elapsed().as_secs_f64(), None)
}

fn main() {
    let args = parse_args();

    let mut selected: Vec<&Variant> = Vec::new();
    for name in &args.solvers {
        if name == "all" {
            for v in REGISTRY.iter() {
                if !selected.iter().any(|s| s.name == v.name) {
                    selected.push(v);
                }
            }
            continue;
        }
        match REGISTRY.iter().find(|v| v.name == *name) {
            Some(v) => {
                if !selected.iter().any(|s| s.name == v.name) {
                    selected.push(v);
                }
            }
            None => {
                eprintln!("unknown solver `{}`; available: all, {}", name, names().join(", "));
                std::process::exit(2);
            }
        }
    }

    println!("find_quad: real train(HiddenQuad) generation, seed {}\n", args.seed);
    println!("{:<14} {:>12} {:>10} {:>14}  {}", "solver", "attempts", "secs", "us/attempt", "result");

    let mut found_line: Option<String> = None;
    let mut found_givens = 0usize;
    for v in selected {
        let (attempts, secs, puzzle) = run(v, args.seed);
        let us = secs * 1e6 / attempts as f64;
        let result = match &puzzle {
            Some(p) => {
                found_givens = p.givens();
                found_line = Some(p.to_line());
                format!("{} givens", p.givens())
            }
            None => "not found (cap)".to_string(),
        };
        println!(
            "{:<14} {:>12} {:>10.3} {:>14.1}  {}",
            v.name, attempts, secs, us, result
        );
    }

    if let Some(line) = found_line {
        println!("\nfound a {found_givens}-given hidden-quad puzzle:");
        println!("{line}");
    } else {
        eprintln!("\nno hidden-quad puzzle found within attempt cap");
        std::process::exit(1);
    }
}
