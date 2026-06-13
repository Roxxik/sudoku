//! Prober work vs propagation toolbox — the warp/off-warp sizing run.
//!
//! Replays one production strip trajectory through every combination of the prober's
//! propagation techniques (naked single, hidden-column, hidden-row+box) and reports the
//! DFS node count each would pay, bucketed by clue count and keep/revert disposition.
//! The trajectory (and so the posed-probe stream) is production's, identical across
//! toolboxes — only the per-probe work changes — so the numbers isolate exactly what the
//! prober's propagation choice costs, the quantity a warp-kernel timing run cannot reveal.
//!
//! Toolbox bits: 0x01 naked, 0x02 hidden-col, 0x04 hidden-row+box, 0x08 LC-row,
//! 0x10 LC-col (32 combinations). LC is split by orientation like the hidden singles:
//! LC-row (box↔row) is in-lane per band, LC-col (box↔column) straddles the bands. The
//! scalar off-warp prober is N+RB (0x05); the SIMT on-warp prober is N+C+RB (0x07,
//! `warp_pass_full`). Neither runs LC (it is the baseline solver's technique); the LC
//! bits model adding it to the prober's per-pass propagation. So `N+RB` vs `N+C+RB` is
//! the off-warp vs on-warp gap, and the LC-row/LC-col rows are the per-orientation delta.
//! CAVEAT: LC without the full hidden-single set is non-confluent — those rows are this
//! kernel order's realization, not a canonical toolbox property (see the module docs).
//!
//! Usage:
//!   cargo run --release -p generator-lab --example proberbox -- [attempts] [seed] [mode] [cap]
//!     mode = lc    (scalar & simt, each with no-LC / +LC-row / +LC-col / +both) [default]
//!          | naked (only the 4 naked-bearing singles toolboxes, no LC)
//!          | all   (all 32; small attempts + node cap, the no-prop ones explode)
//!     cap  = per-probe node budget (0 = unbounded); capped probes are a lower bound.
//!   defaults: attempts=200 seed=1 mode=lc cap=0   (all mode caps at 2000000)

use generator_lab::generate::{ToolboxStat, toolbox_stat};
use generator_lab::probe::toolbox::{SCALAR, SIMT, TOOLBOXES, tb_name};
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{HIDDEN_QUAD, NAKED_PAIR};

/// `which` mask + ordered list of toolbox indices to report, per mode. Bits: 0x01 naked,
/// 0x02 hidden-col, 0x04 hidden-row+box, 0x08 LC-row, 0x10 LC-col.
fn mode_toolboxes(mode: &str) -> (u32, Vec<usize>) {
    let order: Vec<usize> = match mode {
        // The four naked-bearing singles toolboxes (no LC), increasing propagation.
        "naked" => vec![0x01, 0x03, SCALAR, SIMT],
        // scalar and simt, each with no-LC / +LC-row / +LC-col / +both — so the LC-row
        // vs LC-col deltas read down adjacent rows.
        "lc" => vec![
            SCALAR, SCALAR | 0x08, SCALAR | 0x10, SCALAR | 0x18,
            SIMT, SIMT | 0x08, SIMT | 0x10, SIMT | 0x18,
        ],
        // All 32, ordered none -> full.
        _ => (0..TOOLBOXES).collect(),
    };
    let mask = order.iter().fold(0u32, |m, &i| m | (1 << i));
    (mask, order)
}

/// Clue-count bands (givens on the board at the posed gate), high to low. The last few
/// span the uniqueness boundary (~22-32 clues), where the memos put ~85% of prober cost.
const BANDS: [(usize, usize, &str); 6] = [
    (40, 81, ">=40"),
    (33, 39, "33-39"),
    (29, 32, "29-32"),
    (26, 28, "26-28"),
    (23, 25, "23-25"),
    (0, 22, "<=22"),
];

fn band_sum(by_clue: &[u64; 82], lo: usize, hi: usize) -> u64 {
    (lo..=hi.min(81)).map(|c| by_clue[c]).sum()
}

fn report(label: &str, s: &ToolboxStat, order: &[usize]) {
    let att = s.attempts.max(1) as f64;
    let posed = s.posed.max(1) as f64;
    let keeps = s.keeps.max(1) as f64;
    let reverts = s.reverts.max(1) as f64;
    println!("================================================================");
    println!("==== {label}");
    println!(
        "  {} attempts, {} successes ({:.2}/1k)   posed probes {} ({:.1}/att)   keep {} / revert {}",
        s.attempts, s.successes, s.successes as f64 / att * 1000.0, s.posed, s.posed as f64 / att,
        s.keeps, s.reverts,
    );
    println!(
        "  fast-path skips (no probe): ua-caught {}  alts==0 {}  re-force {}   keep-rate {:.1}%",
        s.skip_ua, s.skip_alts0, s.skip_reforce, s.keeps as f64 / posed * 100.0,
    );
    println!();
    // --- Per-toolbox summary. nodes = branch-tree size; passes = warp-tick proxy
    // (one warp_pass_full per pass; util 100% => warp time ~ total passes). -----------
    println!(
        "  {:<16} {:>11} {:>12} {:>11} {:>11} {:>13} {:>8}",
        "toolbox", "nodes/probe", "passes/probe", "passes/node", "passes/keep", "passes/revert",
        "capped%",
    );
    for &t in order {
        let nodes = s.nodes_keep[t] + s.nodes_revert[t];
        let pass = s.passes_keep[t] + s.passes_revert[t];
        let capped = s.capped_keep[t] + s.capped_revert[t];
        println!(
            "  {:<16} {:>11.3} {:>12.3} {:>11.3} {:>11.3} {:>13.3} {:>7.1}%",
            tb_name(t),
            nodes as f64 / posed,
            pass as f64 / posed,
            pass as f64 / nodes.max(1) as f64,
            s.passes_keep[t] as f64 / keeps,
            s.passes_revert[t] as f64 / reverts,
            capped as f64 / posed * 100.0,
        );
    }
    println!();
    // --- Probe distribution by clue band -------------------------------------------
    println!("  probe distribution by clue band (givens on board at gate):");
    println!("  {:<10} {:>10} {:>10} {:>10}", "band", "posed", "keep", "revert");
    for (lo, hi, name) in BANDS {
        let p = band_sum(&s.posed_by_clue, lo, hi);
        if p == 0 {
            continue;
        }
        println!(
            "  {:<10} {:>10} {:>10} {:>10}",
            name, p, band_sum(&s.keep_by_clue, lo, hi), band_sum(&s.revert_by_clue, lo, hi),
        );
    }
    println!();
    // --- passes/probe by clue band, per toolbox (the warp-cost shape) ----------------
    println!("  passes/probe by clue band (keep | revert), per toolbox:");
    print!("  {:<22}", "band ->");
    for (_, _, name) in BANDS {
        print!(" {:>13}", name);
    }
    println!();
    for &t in order {
        // keep row
        print!("  {:<22}", format!("{} keep", tb_name(t)));
        for (lo, hi, _) in BANDS {
            let p = band_sum(&s.keep_by_clue, lo, hi);
            let n = band_sum(&s.passes_keep_by_clue[t], lo, hi);
            print!(" {:>13}", if p == 0 { "-".into() } else { format!("{:.2}", n as f64 / p as f64) });
        }
        println!();
        // revert row
        print!("  {:<22}", format!("{} revert", tb_name(t)));
        for (lo, hi, _) in BANDS {
            let p = band_sum(&s.revert_by_clue, lo, hi);
            let n = band_sum(&s.passes_revert_by_clue[t], lo, hi);
            print!(" {:>13}", if p == 0 { "-".into() } else { format!("{:.2}", n as f64 / p as f64) });
        }
        println!();
    }
    println!();
}

fn main() {
    let mut a = std::env::args().skip(1);
    let attempts: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(200);
    let seed: u64 = a.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let mode: String = a.next().unwrap_or_else(|| "lc".into());
    let (which, order) = mode_toolboxes(&mode);
    // Naked-bearing modes never explode, so leave them uncapped; "all" includes the
    // propagation-poor toolboxes, so cap them.
    let default_cap: u64 = if mode == "all" { 2_000_000 } else { 0 };
    let cap: u64 = a.next().and_then(|s| s.parse().ok()).unwrap_or(default_cap);

    println!("# proberbox: {attempts} attempts, seed {seed}, mode {mode}, node cap {cap}");
    println!("# toolbox bits: 0x01 naked | 0x02 hidden-col | 0x04 hidden-row+box | 0x08 LC-row | 0x10 LC-col");
    println!("# reported: {:?}", order);
    let _ = TOOLBOXES;

    let workload: Vec<(&str, Spec)> = vec![
        ("train(hidden-quad)", Spec::train(HIDDEN_QUAD)),
        ("drill(hidden-quad)", Spec::drill(HIDDEN_QUAD)),
        ("train(naked-pair) [cheap contrast]", Spec::train(NAKED_PAIR)),
    ];
    for (label, spec) in &workload {
        let s = toolbox_stat(seed, spec, attempts, which, cap);
        report(label, &s, &order);
    }
}
