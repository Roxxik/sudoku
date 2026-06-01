//! Generate one actual puzzle for train/drill(HiddenQuad) and print its 81-char
//! line, so it can be fed to core's CLI/verifier to confirm the spec is met.
//!
//! Usage: cargo run --release -p generator-lab --example find -- [--mode train|drill] [--seed S=1] [--max N=200000]

use generator_lab::generator::generate;
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;

fn main() {
    let mut mode = 0u32;
    let mut seed = 1u64;
    let mut max = 200_000usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mode" => {
                mode = match it.next().as_deref() {
                    Some("drill") => 1,
                    _ => 0,
                }
            }
            "--seed" => seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(seed),
            "--max" => max = it.next().and_then(|s| s.parse().ok()).unwrap_or(max),
            _ => {}
        }
    }

    let label = if mode == 0 { "train" } else { "drill" };
    let spec = spec_for_mode(mode);
    let mut rng = Rng::from_seed(seed);
    let t0 = std::time::Instant::now();
    let (puzzle, stats) = generate(&mut rng, &spec, max);
    let elapsed = t0.elapsed();
    let us_per_attempt = elapsed.as_secs_f64() * 1e6 / stats.attempts.max(1) as f64;
    match puzzle {
        Some(p) => {
            println!(
                "{label}(HiddenQuad): found a {}-given puzzle after {} attempts ({us_per_attempt:.2} us/attempt)",
                p.givens, stats.attempts
            );
            println!("{}", p.puzzle.to_line());
            println!("solution: {}", p.solution.to_line());
        }
        None => {
            eprintln!(
                "{label}(HiddenQuad): no puzzle in {} attempts (never-fired {}, not-forced {}, {us_per_attempt:.2} us/attempt)",
                stats.attempts, stats.never_fired, stats.not_forced
            );
            std::process::exit(1);
        }
    }
}
