//! Generate puzzles for a `--force`/`--toolbox` spec by racing seeds through the packed
//! SIMT warp host (`warp_host::GateStream`), W=8 in flight, one attempt per seed. Prints
//! each produced puzzle's 81-char line plus its solution, so they can be fed straight to
//! core's CLI/verifier.
//!
//! This is the SIMT mirror of `find`: identical arguments, identical output. One seed = one
//! attempt and most seeds yield nothing, so reaching `--count` puzzles takes far more than
//! `--count` seeds; which seeds yield is a pure function of the seed (lane-for-lane
//! identical to scalar `find`). That makes this the batch way to fill a persisted seed ->
//! puzzle map: feed a seed range and keep the `(seed, puzzle)` pairs it produces — the
//! seeds that yield nothing are simply absent. Here we feed the contiguous range `base..`;
//! `GateStream` takes any seed iterator, so a custom set of seeds plugs in the same way.
//!
//! Puzzles are streamed to stdout as they finish (warp-completion order, NOT seed order)
//! so an interrupt loses nothing already printed; the stats line lands on stderr at the
//! end. Pipe to `sort` if you want them ordered.
//!
//! Batch generation is the realistic way to USE the SIMT prober — there are always >= 8
//! independent seeds to keep the warp full. `--max` caps the total attempts (= seeds tried)
//! so a low- or zero-yield spec can't run forever (default unlimited); a run that hits it
//! before `--count` puzzles exits non-zero. For a yield-independent fixed-budget SIMT run
//! use `findpar-bench`.
//!
//! Usage: cargo run --release -p generator-lab --example findpar -- \
//!          --force NAME[:COUNT] [--force NAME[:COUNT] ...] \
//!          [--toolbox train|drill|full] [--seed BASE=1] [--count N=1] [--max M=unlimited]
//!
//! NAME is any kind from `spec::kinds::NAMES` (e.g. `x-wing`, `xy-wing`, `hidden-quad`).
//! `--toolbox train` (default) allows the union of each forced target's train-scope;
//! `drill` concedes the simpler same-branch peers; `full` allows the whole ladder.

use generator_lab::cli::FindArgs;
use generator_lab::generate::warp_host::{GateStream, Pumped};

fn main() {
    let args = FindArgs::from_env();
    let spec = args.spec();
    let label = args.label();
    let toolbox = args.toolbox.label();

    let t0 = std::time::Instant::now();
    // Print each puzzle the instant it is produced (out of seed order) so a Ctrl-C loses
    // nothing already emitted. stdout stays clean puzzle data (pipeable to the verifier).
    // Seeds are consumed one-attempt-each until `count` puzzles surface (or `max` attempts).
    let mut stream = GateStream::new(args.base_seed.., &spec);
    let mut found = 0u64;
    while found < args.count && stream.stats().attempts < args.max {
        match stream.pump(4096) {
            Pumped::Found(seed, p) => {
                found += 1;
                println!("seed {seed}: {} ({} givens)", p.puzzle.to_line(), p.givens);
                println!("solution: {}", p.solution.to_line());
            }
            Pumped::StepCountReached => {}
            Pumped::NoMorePuzzles => break,
        }
    }
    let stats = stream.stats();
    let elapsed = t0.elapsed();

    // Summary on stderr, at the end.
    let us_per_attempt = elapsed.as_secs_f64() * 1e6 / stats.attempts.max(1) as f64;
    eprintln!(
        "findpar[{label}] toolbox={toolbox}: {}/{} puzzle(s) in {:.3}s over {} attempts \
         ({us_per_attempt:.2} us/attempt, W=8 parallel; never-fired {}, not-forced {})",
        stats.successes,
        args.count,
        elapsed.as_secs_f64(),
        stats.attempts,
        stats.never_fired,
        stats.not_forced,
    );
    if (stats.successes as u64) < args.count {
        eprintln!(
            "findpar[{label}]: only {}/{} puzzle(s) within --max {} attempts",
            stats.successes, args.count, args.max
        );
        std::process::exit(1);
    }
}
