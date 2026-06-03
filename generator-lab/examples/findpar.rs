//! Generate N puzzles for train/drill(HiddenQuad) by racing W=8 seed streams in
//! parallel through the packed SIMT prober (`warp::find_puzzles`), stopping as soon
//! as N are found. Prints each puzzle's 81-char line plus its solution, so they can
//! be fed straight to core's CLI/verifier.
//!
//! This is the realistic way to USE the SIMT prober — batch generation, where there
//! are always >= 8 independent attempts to keep the warp full. A single puzzle from
//! a single seed is inherently sequential (attempts share one RNG stream, queries
//! within an attempt are sequential), so for that use the scalar `find` example.
//!
//! Usage: cargo run --release -p generator-lab --example findpar -- [--mode train|drill] [--count N=1] [--seed BASE=1]

use generator_lab::spec_for_mode;
use generator_lab::warp::find_puzzles;

fn main() {
    let mut mode = 0u32;
    let mut base = 1u64;
    let mut count = 1usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mode" => mode = if it.next().as_deref() == Some("drill") { 1 } else { 0 },
            "--seed" => base = it.next().and_then(|s| s.parse().ok()).unwrap_or(base),
            "--count" => count = it.next().and_then(|s| s.parse().ok()).unwrap_or(count),
            _ => {}
        }
    }

    let label = if mode == 0 { "train" } else { "drill" };
    let spec = spec_for_mode(mode);
    let t0 = std::time::Instant::now();
    let (puzzles, stats) = find_puzzles(base, &spec, count);
    let elapsed = t0.elapsed();

    // Summary on stderr so stdout is clean puzzle data (pipeable to the verifier).
    let us_per_attempt = elapsed.as_secs_f64() * 1e6 / stats.attempts.max(1) as f64;
    eprintln!(
        "{label}(HiddenQuad): {} puzzle(s) in {:.3}s over {} attempts ({us_per_attempt:.2} us/attempt, W=8 parallel)",
        puzzles.len(),
        elapsed.as_secs_f64(),
        stats.attempts
    );
    for p in &puzzles {
        println!("{} ({} givens)", p.puzzle.to_line(), p.givens);
        println!("solution: {}", p.solution.to_line());
    }
}
