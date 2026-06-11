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
    use generator_lab::generate::{defer_stat, ua_catch_stat};

    let (attempts, seed) = parse_args();
    println!("# deferstat M1+M3 (counter run): {attempts} attempts, base seed {seed}\n");

    for (label, spec) in workload() {
        let s = defer_stat(seed, &spec, attempts);
        report_m1_m3(&label, &s);
    }

    // M-UA-LIB: true 2-digit-UA catch — spec-invariant (reverts are spec-independent), so
    // the two production hidden-quad specs suffice.
    println!("\n################ M-UA-LIB: true 2-digit unavoidable-set catch ################\n");
    for (label, spec) in [
        ("train(hidden-quad)", Spec::train(HIDDEN_QUAD)),
        ("drill(hidden-quad)", Spec::drill(HIDDEN_QUAD)),
    ] {
        let u = ua_catch_stat(seed, &spec, attempts);
        report_ua(label, &u);
    }
}

#[cfg(feature = "count")]
fn report_ua(label: &str, u: &generator_lab::generate::UaCatchStat) {
    println!("==== {label}: {} attempts ====", u.attempts);
    println!(
        "  library: {} boards, {:.1} UAs/board ({:.1} UA4s/board)   soundness false_positive (must be 0): {}",
        u.boards,
        u.lib_uas as f64 / u.boards.max(1) as f64,
        u.lib_ua4 as f64 / u.boards.max(1) as f64,
        u.false_positive,
    );
    let rev_all = u.reverts[0] + u.reverts[1];
    let rnd_all = u.revert_nodes[0] + u.revert_nodes[1];
    let att = u.attempts.max(1) as f64;
    println!(
        "  reverts {} ({:.1}/att): clue>=33 {} / clue<=32 {};  revert-nodes {} ({:.1}/att)",
        rev_all, rev_all as f64 / att, u.reverts[0], u.reverts[1], rnd_all, rnd_all as f64 / att,
    );
    let pc = |n: u64, d: u64| 100.0 * n as f64 / d.max(1) as f64;
    println!("  CATCH RATE (caught / reverts):           BY COUNT                 |          BY NODES (cost)");
    println!(
        "    {:<12} {:>10} {:>10} {:>8}  | {:>10} {:>10} {:>8}",
        "tier", "clue>=33", "clue<=32", "ALL", "clue>=33", "clue<=32", "ALL"
    );
    let row = |name: &str, ct: &[u64; 2], nd: &[u64; 2]| {
        println!(
            "    {:<12} {:>9.1}% {:>9.1}% {:>7.1}% | {:>9.1}% {:>9.1}% {:>7.1}%",
            name,
            pc(ct[0], u.reverts[0]),
            pc(ct[1], u.reverts[1]),
            pc(ct[0] + ct[1], rev_all),
            pc(nd[0], u.revert_nodes[0]),
            pc(nd[1], u.revert_nodes[1]),
            pc(nd[0] + nd[1], rnd_all),
        );
    };
    row("UA4-only", &u.caught_ua4_ct, &u.caught_ua4_nd);
    row("UA4+2digit", &u.caught_all_ct, &u.caught_all_nd);
    print!("  locked-cell census (avg sole-given cells at clue=K):");
    for k in [70usize, 60, 50, 40, 33, 32, 28, 26, 24, 22] {
        if u.locked_n[k] > 0 {
            print!(" {k}:{:.1}", u.locked_sum[k] as f64 / u.locked_n[k] as f64);
        }
    }
    println!("\n  RAWUALIB census clue:avg");
    for k in (17..=80).rev() {
        if u.locked_n[k] > 0 {
            print!(" {k}:{:.2}", u.locked_sum[k] as f64 / u.locked_n[k] as f64);
        }
    }
    println!("\n");
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

    // --- M-UA: probe-skip-by-unavoidable-set sizing ---
    let att = s.attempts.max(1) as f64;
    let tot_nodes_all = tot_nk + tot_nr;
    println!("M-UA  probe-skip-by-unavoidable-set sizing:");
    println!(
        "    prize pool: {:.2} reverts/att  {:.1} revert-nodes/att  ({:.1} total nodes/att; revert-node share {:.1}%)",
        tot_revert as f64 / att,
        tot_nr as f64 / att,
        tot_nodes_all as f64 / att,
        100.0 * tot_nr as f64 / tot_nodes_all.max(1) as f64,
    );
    // Witness-size buckets {<=4,<=6,<=8,<=10,<=12,<=14,<=16,>16}.
    let bucket_w = |size: usize| -> usize {
        match size {
            0..=4 => 0,
            5..=6 => 1,
            7..=8 => 2,
            9..=10 => 3,
            11..=12 => 4,
            13..=14 => 5,
            15..=16 => 6,
            _ => 7,
        }
    };
    let bucketize = |gi: usize| -> ([u64; 8], u64) {
        let mut bk = [0u64; 8];
        let mut tot = 0u64;
        for size in 0..82 {
            let c = if gi == 2 { s.witness_size[0][size] + s.witness_size[1][size] } else { s.witness_size[gi][size] };
            bk[bucket_w(size)] += c;
            tot += c;
        }
        (bk, tot)
    };
    let (_, w_all) = bucketize(2);
    println!(
        "    witnesses {w_all} (= reverts {tot_revert}? {})   UA = solution-vs-alternate cell diff",
        if w_all == tot_revert { "yes" } else { "MISMATCH" }
    );
    println!(
        "      {:<14} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "group(n)", "<=4", "<=6", "<=8", "<=10", "<=12", "<=14", "<=16", ">16"
    );
    for (gname, gi) in [("clue>=33", 0usize), ("clue<=32", 1usize), ("all", 2usize)] {
        let (bk, tot) = bucketize(gi);
        println!(
            "      {:<14} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
            format!("{gname}({tot})"),
            bk[0], bk[1], bk[2], bk[3], bk[4], bk[5], bk[6], bk[7]
        );
    }
    // Cumulative catch-rate of a complete size-<=k UA library, all reverts (the decider).
    let (bk_all, tot_all) = bucketize(2);
    let ub = [4usize, 6, 8, 10, 12, 14, 16];
    let mut run = 0u64;
    let mut cum = String::new();
    for b in 0..7 {
        run += bk_all[b];
        cum.push_str(&format!(" <={}:{:.1}%", ub[b], 100.0 * run as f64 / tot_all.max(1) as f64));
    }
    println!(
        "      ALL cumulative catch (by count):{cum}   (>16 uncaught: {:.1}%)",
        100.0 * bk_all[7] as f64 / tot_all.max(1) as f64
    );
    // Node-weighted: the prober cost a complete size-<=k library actually reclaims.
    let mut nbk = [0u64; 8];
    let mut ntot = 0u64;
    for size in 0..82 {
        let c = s.witness_nodes[0][size] + s.witness_nodes[1][size];
        nbk[bucket_w(size)] += c;
        ntot += c;
    }
    let mut nrun = 0u64;
    let mut ncum = String::new();
    for b in 0..7 {
        nrun += nbk[b];
        ncum.push_str(&format!(" <={}:{:.1}%", ub[b], 100.0 * nrun as f64 / ntot.max(1) as f64));
    }
    println!(
        "      ALL cumulative catch (by NODES):{ncum}   (>16 uncaught: {:.1}% of revert cost)",
        100.0 * nbk[7] as f64 / ntot.max(1) as f64
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
    for (gi, gname) in [(0usize, "clue_ge33"), (1usize, "clue_le32")] {
        print!("  RAWUA {gname} size:count:nodes");
        for size in 0..82 {
            if s.witness_size[gi][size] > 0 {
                print!(" {size}:{}:{}", s.witness_size[gi][size], s.witness_nodes[gi][size]);
            }
        }
        println!();
    }
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
    // tot/vfy us/att drift with machine thermal state; verify% and the keep/revert probe
    // SPLIT are ratios within one walk, so they are the stable, comparable numbers.
    println!(
        "  {:<33} {:>9} {:>6} {:>7} {:>8} {:>8} {:>9} {:>9} {:>9} {:>9}",
        "spec", "tot us/at", "vfy%", "vfy/1k", "vfyfail", "us/vfy", "vfy us/at", "revprb/at", "keepprb/at", "succ/notF"
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
        let revprb_us_per_att = vs.revert_probe_nanos as f64 / 1000.0 / vs.attempts.max(1) as f64;
        let keepprb_us_per_att = vs.keep_probe_nanos as f64 / 1000.0 / vs.attempts.max(1) as f64;
        println!(
            "  {:<33} {:>9.2} {:>5.2}% {:>7.1} {:>7.1}% {:>8.2} {:>9.4} {:>9.3} {:>9.4} {:>4}/{:<4}",
            label,
            us_per_att,
            verify_share_pct,
            vfy_per_1k,
            vfy_fail,
            us_per_vfy,
            vfy_us_per_att,
            revprb_us_per_att,
            keepprb_us_per_att,
            vs.successes,
            vs.not_forced,
        );
    }
    println!(
        "\n  projected SIMT verify share = (vfy us/att) / (SIMT us/att for the same spec).\n  SIMT us/att comes from the warp bench (combobench/simtbench); report the ratio there."
    );
}
