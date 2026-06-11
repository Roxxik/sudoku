//! Three measurement runs on the scalar strip generator (equivalence-pinned to the
//! warp rig, so gate-sequence statistics transfer). All instrumentation, no behavior
//! change.
//!
//!   M1  deferred-strip sizing — prober cost by walk position (clue count) and gate
//!       disposition (keep = unique / revert = non-unique), plus the run-length
//!       distribution of consecutive keeps a deferred batch would exploit.
//!   M3  common-snapshot sizing — prober root-drain share of probe cost, and baseline
//!       closure-drain share of solve cost, per kept gate.
//!   M2  folded-forcing sizing — wall-time share of the cold verify (the
//!       `min_target_uses` avoid walk) in end-to-end generation.
//!
//! M1+M3 are counter runs (PCTR / BAND_CTR / FSTAT), so build them WITH `--features
//! count`. M2 is a wall-time run, so build it WITHOUT (counters perturb timing). The
//! same file serves both: the count build compiles the M1+M3 main, the plain build the
//! M2 main.
//!
//! Workload (per the request): the production rare spec HiddenQuad in train and drill,
//! a NakedPair-class cheap contrast, and three combined train-union specs that force two
//! Expert kinds at once (combobench's builder) — spanning the branch pairings.
//!
//! Usage:
//!   M1+M3:  cargo run --release --features count -p generator-lab --example deferstat -- [attempts=3000] [seed=1]
//!   M2:     cargo run --release            -p generator-lab --example deferstat -- [attempts=3000] [seed=1]

use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{
    HIDDEN_QUAD, JELLYFISH, NAKED_PAIR, NAKED_QUAD, NAKED_TRIPLE, NAMES, SWORDFISH, W_WING,
    XYZ_WING,
};
use generator_lab::{drill_union, train_union};

/// The (label, spec) workload, in report order. The combined pairs carry both a train
/// and a drill variant; the single-kind HiddenQuad likewise (train + drill); the cheap
/// NakedPair contrast is train-only.
fn workload() -> Vec<(String, Spec)> {
    let combo = |forces: &[usize]| {
        forces.iter().map(|&i| NAMES[i]).collect::<Vec<_>>().join(" + ")
    };
    let pairs: [&[usize]; 3] =
        [&[W_WING, JELLYFISH], &[XYZ_WING, NAKED_QUAD], &[SWORDFISH, NAKED_TRIPLE]];
    let mut w: Vec<(String, Spec)> = vec![
        ("train(hidden-quad)".into(), Spec::train(HIDDEN_QUAD)),
        ("drill(hidden-quad)".into(), Spec::drill(HIDDEN_QUAD)),
        ("train(naked-pair) [cheap contrast]".into(), Spec::train(NAKED_PAIR)),
    ];
    for forces in pairs {
        w.push((format!("train[{}]", combo(forces)), train_union(forces)));
        w.push((format!("drill[{}]", combo(forces)), drill_union(forces)));
    }
    w
}

fn parse_args() -> (usize, u64) {
    let mut args = std::env::args().skip(1);
    let attempts: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(3000);
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    (attempts, seed)
}

// ====================================================================================
// M1 + M3 — counter run (feature = "count")
// ====================================================================================
#[cfg(feature = "count")]
fn main() {
    use generator_lab::generate::defer_stat;

    let (attempts, seed) = parse_args();
    println!("# deferstat M1+M3 (counter run): {attempts} attempts, base seed {seed}\n");

    for (label, spec) in workload() {
        let s = defer_stat(seed, &spec, attempts);
        report_m1_m3(&label, &s);
    }
}

/// 8 clue-count buckets from 81 down: bucket 0 = clues 73-80, ..., bucket 7 = clues <= 24.
#[cfg(feature = "count")]
fn bucket_of(clue: usize) -> usize {
    ((80usize.saturating_sub(clue)) / 8).min(7)
}

#[cfg(feature = "count")]
fn bucket_label(b: usize) -> String {
    let hi = 80 - 8 * b;
    let lo = hi - 7;
    if b == 7 { format!("<={hi}", hi = hi) } else { format!("{lo}-{hi}") }
}

#[cfg(feature = "count")]
fn report_m1_m3(label: &str, s: &generator_lab::generate::DeferStat) {
    println!("================================================================================");
    println!("spec: {label}   ({} attempts)", s.attempts);
    println!("================================================================================");

    // --- M1 table, 8 buckets by clue count ---
    let mut b_posed = [0u64; 8];
    let mut b_fast = [0u64; 8];
    let mut b_keep = [0u64; 8];
    let mut b_revert = [0u64; 8];
    let mut b_nk = [0u64; 8];
    let mut b_nr = [0u64; 8];
    let mut b_baserev = [0u64; 8];
    for c in 0..=81usize {
        let b = bucket_of(c);
        b_posed[b] += s.posed[c.min(81)];
        b_fast[b] += s.fastpath[c.min(81)];
        b_keep[b] += s.keeps[c.min(81)];
        b_revert[b] += s.reverts[c.min(81)];
        b_nk[b] += s.nodes_keep[c.min(81)];
        b_nr[b] += s.nodes_revert[c.min(81)];
        b_baserev[b] += s.baseline_revert[c.min(81)];
    }
    let tot_nk: u64 = b_nk.iter().sum();
    let tot_nr: u64 = b_nr.iter().sum();
    let tot_nodes = tot_nk + tot_nr;
    let keep_node_share = 100.0 * tot_nk as f64 / tot_nodes.max(1) as f64;

    println!("M1  prober cost by clue-count bucket (keep = unique probe, revert = non-unique):");
    println!(
        "  {:>7} {:>8} {:>8} {:>8} {:>8} {:>12} {:>11} {:>11}",
        "clues", "posed", "fastskip", "keeps", "reverts", "tot-nodes", "nd/keep", "nd/revert"
    );
    for b in 0..8 {
        if b_posed[b] == 0 && b_fast[b] == 0 {
            continue;
        }
        println!(
            "  {:>7} {:>8} {:>8} {:>8} {:>8} {:>12} {:>11.1} {:>11.1}",
            bucket_label(b),
            b_posed[b],
            b_fast[b],
            b_keep[b],
            b_revert[b],
            b_nk[b] + b_nr[b],
            b_nk[b] as f64 / b_keep[b].max(1) as f64,
            b_nr[b] as f64 / b_revert[b].max(1) as f64,
        );
    }
    let tot_posed: u64 = b_posed.iter().sum();
    let tot_keep: u64 = b_keep.iter().sum();
    let tot_revert: u64 = b_revert.iter().sum();
    let tot_fast: u64 = b_fast.iter().sum();
    let tot_baserev: u64 = b_baserev.iter().sum();
    println!(
        "  TOTAL   posed {tot_posed}  fastskip {tot_fast}  keeps {tot_keep}  reverts {tot_revert}  (baseline-revert-of-keeps {tot_baserev})"
    );
    println!(
        "  >>> keep-verdict node share = {keep_node_share:.1}%  ({tot_nk} keep / {tot_nr} revert nodes)   [M1 decider: <~25% => deferred dead]"
    );

    // --- run-length histogram ---
    let runs: u64 = s.run_hist.iter().sum();
    let run_total_len: u64 = (0..82).map(|k| k as u64 * s.run_hist[k]).sum();
    let mean_run = run_total_len as f64 / runs.max(1) as f64;
    // median run length
    let mut acc = 0u64;
    let mut median = 0usize;
    for k in 0..82 {
        acc += s.run_hist[k];
        if acc * 2 >= runs {
            median = k;
            break;
        }
    }
    println!(
        "  run-length of consecutive keeps (fast-skips + unique probes; non-unique breaks):"
    );
    println!("    runs {runs}  mean {mean_run:.2}  median {median}");
    print!("    hist(len:count):");
    for k in 1..82 {
        if s.run_hist[k] > 0 {
            print!(" {k}:{}", s.run_hist[k]);
        }
    }
    println!();

    // --- M3 ---
    let proot = s.probe_root_passes;
    let ptot = s.probe_total_passes;
    let sdrain = s.solve_drain_passes;
    let stot = s.solve_total_passes;
    println!("M3  common-snapshot drain shares (per kept gate):");
    println!(
        "    prober root-drain / probe total = {:.1}%   (band-passes {proot} / {ptot}; {:.2} / {:.2} per gate over {} gates)",
        100.0 * proot as f64 / ptot.max(1) as f64,
        proot as f64 / s.probe_drain_gates.max(1) as f64,
        ptot as f64 / s.probe_drain_gates.max(1) as f64,
        s.probe_drain_gates,
    );
    println!(
        "    baseline closure-drain / solve total = {:.1}%   (sieve-recomputes {sdrain} / {stot}; {:.2} / {:.2} per gate over {} gates)",
        100.0 * sdrain as f64 / stot.max(1) as f64,
        sdrain as f64 / s.solve_drain_gates.max(1) as f64,
        stot as f64 / s.solve_drain_gates.max(1) as f64,
        s.solve_drain_gates,
    );

    // --- raw, parseable ---
    dump_raw(label, s);
    println!();
}

#[cfg(feature = "count")]
fn dump_raw(label: &str, s: &generator_lab::generate::DeferStat) {
    println!("  RAW[{label}] (csv) clue,posed,fastskip,keeps,reverts,nodes_keep,nodes_revert,baseline_revert");
    for c in (0..=81usize).rev() {
        if s.posed[c] != 0 || s.fastpath[c] != 0 {
            println!(
                "  RAW,{c},{},{},{},{},{},{},{}",
                s.posed[c], s.fastpath[c], s.keeps[c], s.reverts[c], s.nodes_keep[c], s.nodes_revert[c], s.baseline_revert[c]
            );
        }
    }
    print!("  RAWRUN runlen:count");
    for k in 1..82 {
        if s.run_hist[k] > 0 {
            print!(" {k}:{}", s.run_hist[k]);
        }
    }
    println!();
    println!(
        "  RAWM3 probe_root_passes={} probe_total_passes={} probe_gates={} solve_drain_passes={} solve_total_passes={} solve_gates={}",
        s.probe_root_passes, s.probe_total_passes, s.probe_drain_gates, s.solve_drain_passes, s.solve_total_passes, s.solve_drain_gates
    );
}

// ====================================================================================
// M2 — wall-time run (NO counters)
// ====================================================================================
#[cfg(not(feature = "count"))]
fn main() {
    use generator_lab::generate::{run_attempts, verify_share};
    use generator_lab::rng::Rng;

    let (attempts, seed) = parse_args();
    println!("# deferstat M2 (wall-time run, no counters): {attempts} attempts, base seed {seed}\n");
    println!(
        "  {:<40} {:>10} {:>9} {:>9} {:>9} {:>11} {:>10} {:>11}",
        "spec", "tot us/att", "vfy%", "vfy/1k", "vfy-fail%", "us/vfy-call", "vfy us/att", "succ/notF"
    );

    for (label, spec) in workload() {
        // Faithfulness cross-check: the timed mirror must reproduce run_attempts' split.
        let mut rng = Rng::from_seed(seed);
        let (stats, _fp) = run_attempts(&mut rng, &spec, attempts);
        let vs = verify_share(seed, &spec, attempts);
        let never_fired = vs.attempts - vs.reached_verify;
        assert_eq!(vs.successes, stats.successes, "faithfulness: successes [{label}]");
        assert_eq!(vs.not_forced, stats.not_forced, "faithfulness: not_forced [{label}]");
        assert_eq!(never_fired, stats.never_fired, "faithfulness: never_fired [{label}]");

        let total_us = vs.total_nanos as f64 / 1000.0;
        let verify_us = vs.verify_nanos as f64 / 1000.0;
        let us_per_att = total_us / vs.attempts.max(1) as f64;
        let verify_share_pct = 100.0 * vs.verify_nanos as f64 / vs.total_nanos.max(1) as f64;
        let vfy_per_1k = 1000.0 * vs.reached_verify as f64 / vs.attempts.max(1) as f64;
        let vfy_fail = 100.0 * vs.not_forced as f64 / vs.reached_verify.max(1) as f64;
        let us_per_vfy = verify_us / vs.reached_verify.max(1) as f64;
        let vfy_us_per_att = verify_us / vs.attempts.max(1) as f64;
        println!(
            "  {:<40} {:>10.3} {:>8.2}% {:>9.1} {:>8.1}% {:>11.3} {:>10.4} {:>5}/{:<5}",
            label,
            us_per_att,
            verify_share_pct,
            vfy_per_1k,
            vfy_fail,
            us_per_vfy,
            vfy_us_per_att,
            vs.successes,
            vs.not_forced,
        );
    }
    println!(
        "\n  projected SIMT verify share = (vfy us/att) / (SIMT us/att for the same spec).\n  SIMT us/att comes from the warp bench (combobench/simtbench); report the ratio there."
    );
}
