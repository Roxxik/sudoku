//! Interleaved A/B of the cross-stall harder-ladder memo on the production SIMT warp.
//! Three [`MemoMode`] variants run over the SAME bounded seed range in alternating,
//! order-rotated chunks inside one binary (the drift-proof protocol):
//!
//!   Off      - no memo: every stall rescans every unit and digit (the floor).
//!   Subsets  - subset unit-scans memoized, fishes fully rescanned (pre-fish baseline).
//!   Full     - subset + per-digit fish memo (production).
//!
//! So `Subsets/Off` is the subset memo's standing win, and `Full/Subsets` ISOLATES the
//! per-digit fish memo's marginal effect — the thing this harness exists to measure. Every
//! mode is exact (first-fire-preserving), so all three must produce the IDENTICAL puzzle
//! set; the per-variant order-independent fingerprint is the soundness guard, the time
//! ratios the verdict. With `--features count`, a final clean Full pass prints the memo's
//! per-family scan-skip share.
//!
//! Same `--force NAME[:COUNT]` / `--toolbox train|drill|full` spec interface as
//! `find`/`findpar`/`findpar-bench`. The seed range IS the per-variant attempt budget
//! (one seed = one attempt), so the work is fixed and yield-independent.
//!
//! Usage:
//!   cargo run --release -p generator-lab --example laddermemoab -- \
//!       --force NAME[:COUNT] [--force ...] [--toolbox train|drill|full] \
//!       [--per-variant 40000] [--chunk 2000] [--seed 1]

use generator_lab::cli::{Toolbox, build_spec, parse_force, spec_label};
use generator_lab::fingerprint::grid_fp;
use generator_lab::generate::warp_host::{GateStream, Pumped};
use generator_lab::solve::simt::MemoMode;

/// Pump `s` until its cumulative attempt counter reaches `target` (or the bounded seed
/// feed drains), XOR-folding each found puzzle's fingerprint into `fp`. Returns the wall
/// time spent in this chunk. One tick per `pump` so the chunk boundary is exact.
fn run_chunk<I: Iterator<Item = u64>>(s: &mut GateStream<I>, target: usize, fp: &mut u64) -> f64 {
    let t0 = std::time::Instant::now();
    while s.stats().attempts < target {
        match s.pump(1) {
            Pumped::Found(_, p) => *fp ^= grid_fp(&p.puzzle.0),
            Pumped::StepCountReached => {}
            Pumped::NoMorePuzzles => break,
        }
    }
    t0.elapsed().as_secs_f64()
}

fn main() {
    let mut forces: Vec<(usize, u16)> = Vec::new();
    let mut toolbox = Toolbox::Train;
    let mut per_variant = 40_000usize;
    let mut chunk = 2_000usize;
    let mut base_seed = 1u64;

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--force" => match parse_force(&it.next().unwrap_or_default()) {
                Ok(f) => forces.push(f),
                Err(msg) => {
                    eprintln!("{msg}");
                    std::process::exit(2);
                }
            },
            "--toolbox" => toolbox = Toolbox::parse(it.next().as_deref()),
            "--per-variant" => {
                per_variant = it.next().and_then(|s| s.parse().ok()).unwrap_or(per_variant)
            }
            "--chunk" => chunk = it.next().and_then(|s| s.parse().ok()).unwrap_or(chunk),
            "--seed" => base_seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(base_seed),
            _ => {}
        }
    }

    if forces.is_empty() {
        eprintln!("need at least one --force NAME[:COUNT]");
        std::process::exit(2);
    }

    let spec = build_spec(&forces, toolbox);
    let label = spec_label(&forces);
    let range = base_seed..base_seed + per_variant as u64;
    println!(
        "laddermemoab[{label}] toolbox={}: {per_variant}/variant in {chunk}-attempt chunks, seed {base_seed}",
        toolbox.label(),
    );

    const MODES: [(MemoMode, &str); 3] =
        [(MemoMode::Off, "off"), (MemoMode::Subsets, "subset"), (MemoMode::Full, "full")];
    let mut streams: Vec<GateStream<_>> =
        MODES.iter().map(|&(m, _)| GateStream::new_opts(range.clone(), &spec, m)).collect();
    let mut fps = [0u64; 3];
    let mut times = [0.0f64; 3];

    // Interleave the three variants in chunks, rotating which goes first per chunk so slow
    // drift cancels; chunk 0 is the warmup and is excluded from the timed totals.
    let chunks = per_variant.div_ceil(chunk);
    for c in 0..chunks {
        let target = ((c + 1) * chunk).min(per_variant);
        let measure = c > 0;
        for step in 0..3 {
            let v = (c + step) % 3; // rotated order
            let dt = run_chunk(&mut streams[v], target, &mut fps[v]);
            if measure {
                times[v] += dt;
            }
        }
    }

    // Soundness: the memo is exact, so all three variants produce the SAME puzzle set, and
    // the order-independent XOR-fold over the complete (identical, bounded) seed range must
    // match across all three.
    let sound = fps[0] == fps[1] && fps[1] == fps[2];

    // The timed attempts exclude the warmup chunk, so divide by (per_variant - chunk0).
    let warmup = chunk.min(per_variant);
    let timed_att = (per_variant - warmup).max(1) as f64;
    let us: Vec<f64> = times.iter().map(|&t| t * 1e6 / timed_att).collect();
    for (i, &(_, name)) in MODES.iter().enumerate() {
        let s = streams[i].stats();
        println!(
            "  {name:<7} {:>7.2} us/att   yield {} / {}",
            us[i], s.successes, s.attempts
        );
    }
    println!(
        "  ratios: subset/off {:.4} ({:+.2}%)   full/subset {:.4} ({:+.2}%)   full/off {:.4} ({:+.2}%)",
        us[1] / us[0],
        100.0 * (us[1] / us[0] - 1.0),
        us[2] / us[1],
        100.0 * (us[2] / us[1] - 1.0),
        us[2] / us[0],
        100.0 * (us[2] / us[0] - 1.0),
    );
    println!(
        "  fp: {:#018x}  puzzles: {}",
        fps[2],
        if sound { "IDENTICAL across all modes" } else { "MISMATCHED  <-- SOUNDNESS VIOLATION" },
    );

    // Memo bite (one clean Full pass over the same range, counters reset around it). The
    // skip share is a property of the workload, independent of the interleaved timing.
    #[cfg(feature = "count")]
    {
        generator_lab::solve::lstat_reset();
        let mut s = GateStream::new_opts(range.clone(), &spec, MemoMode::Full);
        loop {
            match s.pump(4096) {
                Pumped::Found(..) | Pumped::StepCountReached => {}
                Pumped::NoMorePuzzles => break,
            }
        }
        let l = generator_lab::solve::lstat_snapshot();
        let share = |run: u64, skip: u64| 100.0 * skip as f64 / (run + skip).max(1) as f64;
        println!(
            "  ladder: {} entries   subset-scans {} run / {} skipped ({:.1}% skip)   fish-scans {} run / {} skipped ({:.1}% skip)   fish-pos digit rebuilds {}",
            l[0],
            l[1],
            l[2],
            share(l[1], l[2]),
            l[3],
            l[4],
            share(l[3], l[4]),
            l[5],
        );
    }
}
