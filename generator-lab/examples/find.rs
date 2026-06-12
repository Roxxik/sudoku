//! Generate one actual puzzle per seed (scalar) for a `--force`/`--toolbox` spec and
//! print each 81-char line plus its solution, so they can be fed to core's CLI/verifier
//! to confirm the spec is met.
//!
//! This is the scalar mirror of `findpar`: identical arguments, identical seed -> puzzle
//! relation (each seed run to its first success), one puzzle per seed over the range
//! `seed..seed+count`. `findpar` races the same seeds through the W=8 SIMT warp; pick this
//! one for a single puzzle from a single seed (a single attempt stream is inherently
//! sequential, so the warp can't help). For a yield-independent, fixed-budget measurement
//! use `findpar-bench`.
//!
//! Usage: cargo run --release -p generator-lab --example find -- \
//!          --force NAME[:COUNT] [--force NAME[:COUNT] ...] \
//!          [--toolbox train|drill|full] [--seed BASE=1] [--count N=1] [--max M=1000000]
//!
//! NAME is any kind from `spec::kinds::NAMES` (e.g. `x-wing`, `swordfish`, `jellyfish`,
//! `hidden-quad`). `--toolbox train` (default) allows the union of each forced target's
//! train-scope; `drill` concedes the simpler same-branch peers; `full` allows the whole
//! ladder. `--max` is the per-seed attempt cap (a seed that doesn't yield within it is
//! reported on stderr and skipped).

use generator_lab::cli::FindArgs;
use generator_lab::generate::generate;
use generator_lab::rng::Rng;

fn main() {
    let args = FindArgs::from_env();
    let spec = args.spec();
    let label = args.label();
    let toolbox = args.toolbox.label();

    let t0 = std::time::Instant::now();
    let mut total_attempts = 0u64;
    let mut found = 0u64;
    let mut failed = 0u64;
    for seed in args.base_seed..args.base_seed + args.count {
        let mut rng = Rng::from_seed(seed);
        let (puzzle, stats) = generate(&mut rng, &spec, args.max);
        total_attempts += stats.attempts as u64;
        match puzzle {
            Some(p) => {
                found += 1;
                // stdout stays clean puzzle data (pipeable to the verifier).
                println!("seed {seed}: {} ({} givens)", p.puzzle.to_line(), p.givens);
                println!("solution: {}", p.solution.to_line());
            }
            None => {
                failed += 1;
                eprintln!(
                    "seed {seed}: no puzzle in {} attempts (never-fired {}, not-forced {})",
                    stats.attempts, stats.never_fired, stats.not_forced
                );
            }
        }
    }
    let elapsed = t0.elapsed();
    let us_per_attempt = elapsed.as_secs_f64() * 1e6 / total_attempts.max(1) as f64;
    eprintln!(
        "find[{label}] toolbox={toolbox}: {found} puzzle(s) for {} seed(s) in {:.3}s over {total_attempts} attempts ({us_per_attempt:.2} us/attempt, scalar)",
        args.count,
        elapsed.as_secs_f64(),
    );
    if failed > 0 {
        std::process::exit(1);
    }
}
