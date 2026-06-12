//! Generate puzzles (scalar) for a `--force`/`--toolbox` spec, one attempt per seed, and
//! print each 81-char line plus its solution, so they can be fed to core's CLI/verifier to
//! confirm the spec is met.
//!
//! One seed = one attempt: seeds are consumed from `--seed` upward, a single strip walk
//! each, and most yield nothing — so reaching `--count` puzzles tries far more than
//! `--count` seeds. This is the scalar mirror of `findpar`: identical arguments, identical
//! seed -> attempt relation (each seed's single walk is a pure function of the seed,
//! lane-for-lane identical to the SIMT path). `findpar` races the same seeds through the
//! W=8 SIMT warp; for a yield-independent, fixed-budget measurement use `findpar-bench`.
//!
//! Usage: cargo run --release -p generator-lab --example find -- \
//!          --force NAME[:COUNT] [--force NAME[:COUNT] ...] \
//!          [--toolbox train|drill|full] [--seed BASE=1] [--count N=1] [--max M=unlimited]
//!
//! NAME is any kind from `spec::kinds::NAMES` (e.g. `x-wing`, `swordfish`, `jellyfish`,
//! `hidden-quad`). `--toolbox train` (default) allows the union of each forced target's
//! train-scope; `drill` concedes the simpler same-branch peers; `full` allows the whole
//! ladder. `--count` is how many puzzles to produce; `--max` caps the total attempts (=
//! seeds tried) so a low- or zero-yield spec can't run forever (default unlimited). A run
//! that hits `--max` before `--count` puzzles exits non-zero.

use generator_lab::cli::FindArgs;
use generator_lab::generate::{AttemptResult, attempt};
use generator_lab::rng::Rng;

fn main() {
    let args = FindArgs::from_env();
    let spec = args.spec();
    let label = args.label();
    let toolbox = args.toolbox.label();

    let t0 = std::time::Instant::now();
    let mut attempts = 0u64;
    let mut found = 0u64;
    let mut never_fired = 0u64;
    let mut not_forced = 0u64;
    let mut seed = args.base_seed;
    // One seed = one attempt; keep consuming seeds until `count` puzzles surface (or the
    // `max` attempt cap is hit). Most seeds yield nothing.
    while found < args.count && (attempts as usize) < args.max {
        let mut rng = Rng::from_seed(seed);
        attempts += 1;
        match attempt(&mut rng, &spec) {
            AttemptResult::Success(p) => {
                found += 1;
                // stdout stays clean puzzle data (pipeable to the verifier).
                println!("seed {seed}: {} ({} givens)", p.puzzle.to_line(), p.givens);
                println!("solution: {}", p.solution.to_line());
            }
            AttemptResult::NotForced => not_forced += 1,
            AttemptResult::NeverFired => never_fired += 1,
        }
        seed += 1;
    }
    let elapsed = t0.elapsed();
    let us_per_attempt = elapsed.as_secs_f64() * 1e6 / attempts.max(1) as f64;
    eprintln!(
        "find[{label}] toolbox={toolbox}: {found}/{} puzzle(s) in {:.3}s over {attempts} attempts \
         ({us_per_attempt:.2} us/attempt, scalar; never-fired {never_fired}, not-forced {not_forced})",
        args.count,
        elapsed.as_secs_f64(),
    );
    if found < args.count {
        eprintln!(
            "find[{label}]: only {found}/{} puzzle(s) within --max {} attempts",
            args.count, args.max
        );
        std::process::exit(1);
    }
}
