//! `branch_lane` frame-fusion criterion microbench.
//!
//! `branch_lane` snapshots a stalled lane's board straight into its pushed `Frame`. The
//! strided SoA read is the design's irreducible per-branch clone; the old form copied that
//! 31-word board a *second* time (local -> `Vec` slot). Fusing the second copy removed
//! ~111 instructions per branch node (dominated by 82 redundant memory writes). This is the
//! standing criterion that guards that cost — see `docs/FRAME-FUSION.md` for the A/B that
//! proved it (Intel SDE `-mix`, base 245 -> fused 134 insts/call).
//!
//! Corpus: realistic pre-branch boards harvested from a real probe corpus (each probe is
//! driven to its first stall, then snapshotted — the exact state `branch_lane` sees).
//!
//! Usage:
//!   cargo run --release -p generator-lab --example framefusebench -- [corpus_att=4000] [iters=40000000] [spec=0]
//!
//! SDE per-call instruction count (the deterministic criterion; build first, then run once
//! under SDE and read the `branch_lane` row from the FUNCTION TOTALS — icount / #times).
//! Build WITH `--features profiling` so `branch_cell` stays a separate symbol — otherwise
//! LLVM inlines it into `branch_lane` and the row reads ~260 (134 fused body + ~124
//! branch_cell) instead of the clean 134:
//!   SDE=$HOME/opt/sde-external-10.8.0-2026-03-15-lin/sde64
//!   cargo build --release --features profiling -p generator-lab --example framefusebench
//!   "$SDE" -mix -omix mix.txt -- target/release/examples/framefusebench 400 1000000
//!   awk '/branch_lane / && $5>0 {print $2/$5" insts/call"}' mix.txt   # ~134.0 (was 245.0)
//! (add `-chip-check-disable` if SDE rejects a host AVX-512 instruction.)

use generator_lab::generate::warp_host::collect_probes;
use generator_lab::solve::simt::{BranchInput, collect_branch_inputs, run_branch_bench};
use generator_lab::subset_spec_for_mode;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let corpus_att: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(4000);
    let iters: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40_000_000);
    let spec_mode: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let spec = subset_spec_for_mode(spec_mode);

    // Harvest the branch-point corpus once.
    let probes: Vec<_> = collect_probes(1, &spec, corpus_att).into_iter().map(|(p, _)| p).collect();
    let inputs: Vec<BranchInput> = collect_branch_inputs(&probes);
    assert!(!inputs.is_empty(), "no branch points harvested — raise corpus_att");

    // Corpus shape: mean live candidates per input (board fill proxy), for context.
    let mut cand_sum = 0u64;
    for (sr, su) in &inputs {
        for d in 0..9 {
            for b in 0..3 {
                cand_sum += (sr[d][b] & su[b]).count_ones() as u64;
            }
        }
    }
    println!(
        "framefusebench spec={spec_mode}: corpus {} probes -> {} branch points  (mean {:.1} live candidates/board)",
        probes.len(),
        inputs.len(),
        cand_sum as f64 / inputs.len() as f64,
    );

    // Warmup, then three measured runs in one shell (per the project benchmarking method).
    let _ = run_branch_bench(&inputs, (iters / 10).max(1));
    let mut acc = 0u64;
    println!("  branch_lane wall-clock ({iters} ops/run, includes fixed restore+checksum overhead):");
    for run in 0..3 {
        let t = Instant::now();
        acc ^= run_branch_bench(&inputs, iters);
        let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
        println!("    run {run}: {ns:>7.3} ns/op");
    }
    std::hint::black_box(acc);
    println!("  checksum={acc:#018x}");
    println!(
        "\n  The verdict is the SDE -mix per-call instruction count, not this wall-clock\n  (the load+checksum overhead dilutes it). See the header for the SDE one-liner."
    );
}
