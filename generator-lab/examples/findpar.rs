//! Generate one puzzle per seed for train/drill(HiddenQuad) by racing the seeds through
//! the packed SIMT warp host (`warp_host::GateStream`), W=8 in flight. Prints each
//! seed's 81-char puzzle line plus its solution, so they can be fed straight to core's
//! CLI/verifier.
//!
//! The seed -> puzzle relation is a pure function of the seed (each seed is run to its
//! first success, identical to scalar `find` from that seed), so this is the batch way
//! to fill a persisted seed -> puzzle map: pass the seeds that don't have a puzzle yet.
//! Here we use the contiguous range `base..base+count`; `GateStream` takes any seed
//! iterator, so a non-contiguous set of missing seeds plugs in the same way.
//!
//! Puzzles are streamed to stdout as they finish (warp-completion order, NOT seed order)
//! so an interrupt loses nothing already printed; the stats line lands on stderr at the
//! end. Pipe to `sort` if you want them ordered.
//!
//! Batch generation is the realistic way to USE the SIMT prober — there are always >= 8
//! independent seeds to keep the warp full. A single puzzle from a single seed is
//! inherently sequential (attempts share one RNG stream, queries within an attempt are
//! sequential), so for that use the scalar `find` example.
//!
//! Usage: cargo run --release -p generator-lab --example findpar -- \
//!          [--target NAME=hidden-quad] [--mode train|drill] [--count N=1] [--seed BASE=1]
//!
//! NAME is any kind from `spec::kinds::NAMES` (e.g. `x-wing`, `xy-wing`, `hidden-quad`);
//! the target determines its branch (train allows simpler same-branch peers, drill
//! concedes them).

use generator_lab::expert_spec;
use generator_lab::generate::warp_host::{GateStream, Pumped};
use generator_lab::spec::kinds::NAMES;

fn main() {
    let mut target_name = "hidden-quad".to_string();
    let mut drill = false;
    let mut base = 1u64;
    let mut count = 1u64;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => target_name = it.next().unwrap_or(target_name),
            "--mode" => drill = matches!(it.next().as_deref(), Some("drill")),
            "--seed" => base = it.next().and_then(|s| s.parse().ok()).unwrap_or(base),
            "--count" => count = it.next().and_then(|s| s.parse().ok()).unwrap_or(count),
            _ => {}
        }
    }

    let target = NAMES.iter().position(|&n| n == target_name).unwrap_or_else(|| {
        eprintln!("unknown target {target_name:?}; known: {}", NAMES.join(", "));
        std::process::exit(2);
    });
    let label = if drill { "drill" } else { "train" };
    let spec = expert_spec(target, drill);
    let t0 = std::time::Instant::now();
    // Print each puzzle the instant it is produced (out of seed order) so a Ctrl-C loses
    // nothing already emitted. stdout stays clean puzzle data (pipeable to the verifier).
    let mut stream = GateStream::new(base..base + count, &spec);
    loop {
        match stream.pump(4096) {
            Pumped::Found(seed, p) => {
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
        "{label}({target_name}): {} puzzle(s) for {} seed(s) in {:.3}s over {} attempts ({us_per_attempt:.2} us/attempt, W=8 parallel)",
        stats.successes,
        count,
        elapsed.as_secs_f64(),
        stats.attempts
    );
}
