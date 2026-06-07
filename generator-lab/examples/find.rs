//! Generate one actual puzzle for a train/drill target and print its 81-char line,
//! so it can be fed to core's CLI/verifier to confirm the spec is met.
//!
//! Usage: cargo run --release -p generator-lab --example find -- \
//!          [--target NAME=hidden-quad] [--mode train|drill] [--seed S=1] [--max N=1000000]
//!
//! NAME is any kind name from `spec::kinds::NAMES` (e.g. `x-wing`, `swordfish`,
//! `jellyfish`, `hidden-quad`). The target determines its branch; train allows the
//! simpler same-branch peers, drill concedes them.

use generator_lab::expert_spec;
use generator_lab::generate::generate;
use generator_lab::rng::Rng;
use generator_lab::spec::kinds::NAMES;

fn main() {
    let mut target_name = "hidden-quad".to_string();
    let mut drill = false;
    let mut seed = 1u64;
    let mut max = 1_000_000usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => target_name = it.next().unwrap_or(target_name),
            "--mode" => drill = matches!(it.next().as_deref(), Some("drill")),
            "--seed" => seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(seed),
            "--max" => max = it.next().and_then(|s| s.parse().ok()).unwrap_or(max),
            _ => {}
        }
    }

    let target = NAMES.iter().position(|&n| n == target_name).unwrap_or_else(|| {
        eprintln!("unknown target {target_name:?}; known: {}", NAMES.join(", "));
        std::process::exit(2);
    });
    let mode = if drill { "drill" } else { "train" };

    let spec = expert_spec(target, drill);
    let mut rng = Rng::from_seed(seed);
    let t0 = std::time::Instant::now();
    let (puzzle, stats) = generate(&mut rng, &spec, max);
    let elapsed = t0.elapsed();
    let us_per_attempt = elapsed.as_secs_f64() * 1e6 / stats.attempts.max(1) as f64;
    match puzzle {
        Some(p) => {
            println!(
                "{mode}({target_name}): found a {}-given puzzle after {} attempts ({us_per_attempt:.2} us/attempt)",
                p.givens, stats.attempts
            );
            println!("{}", p.puzzle.to_line());
            println!("solution: {}", p.solution.to_line());
        }
        None => {
            eprintln!(
                "{mode}({target_name}): no puzzle in {} attempts (never-fired {}, not-forced {}, {us_per_attempt:.2} us/attempt)",
                stats.attempts, stats.never_fired, stats.not_forced
            );
            std::process::exit(1);
        }
    }
}
