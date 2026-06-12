//! Generate one puzzle per seed for a `--force`/`--toolbox` spec by racing the seeds
//! through the packed SIMT warp host (`warp_host::GateStream`), W=8 in flight. Prints each
//! seed's 81-char puzzle line plus its solution, so they can be fed straight to core's
//! CLI/verifier.
//!
//! This is the SIMT mirror of `find`: identical arguments, identical output. The seed ->
//! puzzle relation is a pure function of the seed (each seed is run to its first success,
//! lane-for-lane identical to scalar `find` from that seed), so this is the batch way to
//! fill a persisted seed -> puzzle map: pass the starting seeds that don't have a puzzle
//! yet. Here we use the contiguous range `base..`; `GateStream` takes any seed iterator,
//! so a non-contiguous set of missing seeds plugs in the same way.
//!
//! Puzzles are streamed to stdout as they finish (warp-completion order, NOT seed order)
//! so an interrupt loses nothing already printed; the stats line lands on stderr at the
//! end. Pipe to `sort` if you want them ordered.
//!
//! Batch generation is the realistic way to USE the SIMT prober — there are always >= 8
//! independent seeds to keep the warp full. A single puzzle from a single seed is
//! inherently sequential (attempts share one RNG stream, queries within an attempt are
//! sequential), so for that use the scalar `find` example. `--max` is accepted for
//! `find` parity but ignored: `findpar` has no per-seed cap (each seed runs to its first
//! success) — for a bounded SIMT run use `findpar-bench`.
//!
//! Usage: cargo run --release -p generator-lab --example findpar -- \
//!          --force NAME[:COUNT] [--force NAME[:COUNT] ...] \
//!          [--toolbox train|drill|full] [--seed BASE=1] [--count N=1]
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
    let mut stream = GateStream::new(args.base_seed.., &spec);
    let mut found = 0;
    while found < args.count {
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
        "findpar[{label}] toolbox={toolbox}: {} puzzle(s) for {} seed(s) in {:.3}s over {} attempts ({us_per_attempt:.2} us/attempt, W=8 parallel)",
        stats.successes,
        args.count,
        elapsed.as_secs_f64(),
        stats.attempts
    );
}
