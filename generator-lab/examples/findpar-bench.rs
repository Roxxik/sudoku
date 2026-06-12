//! `findpar-bench` -- the benchmarking sibling of `find`/`findpar`. Same `--force`/
//! `--toolbox` spec interface, but instead of stopping at a puzzle count it runs a FIXED
//! attempt budget (lanes x per_lane seeds, one attempt each) through the unified W=8 SIMT
//! warp. Fixed work makes the per-attempt cost yield-independent: it measures cleanly even
//! at zero yield, where `findpar` would run forever, and the measured yield
//! (successes/attempts) projects the average s/puzzle.
//!
//! Built for finding a generator codepath worth optimizing: one that is both slow per
//! attempt AND rare (many attempts per puzzle), so the warp's average time to produce one
//! puzzle is large (> 1s is the threshold of interest). Combining requirements with
//! several `--force` kinds (and any kind at count > 1) keeps the per-attempt solver cost
//! high (the baseline ladder can't fast-path a Forced kind) while making the puzzle much
//! rarer -- exactly the slow+rare codepath we want to surface, e.g. "require a W-Wing AND
//! a naked triple" or "require two X-Wings and a jellyfish".
//!
//! Usage:
//!   cargo run --release -p generator-lab --example findpar-bench -- \
//!       --force NAME[:COUNT] [--force NAME[:COUNT] ...] \
//!       [--toolbox train|drill|full] [--lanes 8] [--per-lane 100000] [--seed 1]
//!
//! NAME is any kind from `spec::kinds::NAMES` (e.g. `w-wing`, `naked-triple`,
//! `jellyfish`). COUNT defaults to 1. `--toolbox train` (default) allows the
//! union of each forced target's train-scope (Trunk + simpler-or-equal peers in
//! that target's branch); `--toolbox drill` concedes those simpler same-branch
//! peers instead of allowing them (the multi-force drill generalization — each
//! forced kind fires in the baseline trace rather than being fast-pathed by a
//! simpler peer); `--toolbox full` allows the entire 16-kind ladder (more
//! substitutes => rarer).

use generator_lab::cli::{Toolbox, build_spec, parse_force, spec_label};
use generator_lab::generate::warp_host::{GateStream, Pumped};

/// Whole-run rdtsc envelope (same span as the wall clock) so the warp's per-phase cycle
/// buckets can be expressed as a true fraction of total runtime — the leftover (host
/// tick/pump plumbing + rdtsc/instrumentation overhead) surfaces as a measured
/// `unaccounted` slice instead of a silently-shrunk denominator. 0 off the `count`+x86
/// path (the per-phase counters are zero there too, so the breakdown is just skipped).
#[cfg(all(feature = "count", target_arch = "x86_64"))]
#[inline]
fn rdtsc() -> u64 {
    // SAFETY: _rdtsc is always available on x86_64.
    unsafe { core::arch::x86_64::_rdtsc() }
}
#[cfg(not(all(feature = "count", target_arch = "x86_64")))]
#[inline]
#[allow(dead_code)] // only called from the `count` reporting block
fn rdtsc() -> u64 {
    0
}

fn main() {
    let mut forces: Vec<(usize, u16)> = Vec::new();
    let mut toolbox = Toolbox::Train;
    let mut lanes = 8usize;
    let mut per_lane = 100_000usize;
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
            "--lanes" => lanes = it.next().and_then(|s| s.parse().ok()).unwrap_or(lanes),
            "--per-lane" => per_lane = it.next().and_then(|s| s.parse().ok()).unwrap_or(per_lane),
            "--seed" => base_seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(base_seed),
            _ => {}
        }
    }

    if forces.is_empty() {
        eprintln!("need at least one --force NAME[:COUNT]");
        std::process::exit(2);
    }

    // Build the toolbox spec around the forced kinds (the shared `cli` builder honors any
    // requested COUNT > 1).
    let spec = build_spec(&forces, toolbox);
    let label = spec_label(&forces);
    let toolbox = toolbox.label();
    let total = lanes * per_lane;
    println!(
        "findpar-bench[{label}] toolbox={toolbox}: {lanes} lanes x {per_lane} = {total} attempts"
    );

    #[cfg(feature = "count")]
    {
        generator_lab::repr::banded::psg_reset();
        generator_lab::solve::uwstat_reset();
        generator_lab::generate::warp_host::phstat_reset();
    }
    // Fixed work: one seed = one attempt, so a bounded seed range IS the attempt budget —
    // feed exactly `total` seeds and drain (no outside per-tick cap, no overshoot; the feed
    // bounds the work, and it terminates even for a combo that never yields). Fold an
    // order-independent fingerprint over the produced puzzles so two builds match iff they
    // produce the same puzzle set.
    #[cfg(feature = "count")]
    let env0 = rdtsc();
    let t0 = std::time::Instant::now();
    let mut stream = GateStream::new(base_seed..base_seed + total as u64, &spec);
    let mut combo_fp = 0u64; // XOR-fold of per-puzzle fps: order-independent
    loop {
        match stream.pump(4096) {
            Pumped::Found(_, p) => combo_fp ^= generator_lab::fingerprint::grid_fp(&p.puzzle.0),
            Pumped::StepCountReached => {}
            Pumped::NoMorePuzzles => break, // seed feed drained: all `total` attempts done
        }
    }
    let dt = t0.elapsed();
    #[cfg(feature = "count")]
    let env_cycles = rdtsc().wrapping_sub(env0);
    let stats = stream.stats();
    #[cfg(feature = "count")]
    {
        let h = generator_lab::repr::banded::psg_snapshot();
        let total: u64 = h.iter().sum();
        let pct: Vec<String> =
            h.iter().map(|&c| format!("{:.1}%", 100.0 * c as f64 / total.max(1) as f64)).collect();
        println!("  place_single_group group-size histogram (cells): {h:?}");
        println!("    share: {}", pct.join(" "));
        // --- profiler-style runtime attribution (us/att + % of total wall) -----------
        // Read it the way perf would: one attempt start to finish, every slice of wall
        // time named. The kernel `warp_pass_full` advances ALL active lanes in ONE shared
        // SIMD pass, so probe and baseline are not separate passes — they ride the same
        // pass — and the kernel's cost is split between them by LANE-PASS COUNT (each
        // tick's active lanes tallied by phase). The scalar tail is timed directly: engine
        // service per phase, host coroutine resume (fill/ua/strip/verify). The whole-run
        // rdtsc envelope (`env_cyc`, same span as the wall clock) turns the leftover — host
        // tick/pump plumbing + rdtsc/instrumentation overhead — into a measured
        // `unaccounted` slice, so every row is a true fraction of total runtime.
        let uw = generator_lab::solve::uwstat_snapshot();
        let lanes_const = generator_lab::probe::simt::LANES;
        let ph = generator_lab::generate::warp_host::phstat_snapshot();
        let attempts = stats.attempts.max(1) as f64;

        let (probe_lp, base_lp) = (ph[0] as f64, ph[1] as f64);
        let lane_passes = (probe_lp + base_lp).max(1.0);
        let probe_pass_share = probe_lp / lane_passes;

        // Each named cycle bucket. probe/baseline each = warp_pass (kernel lane-pass share)
        // + service (engine). fill/ua-build/verify are timed once per attempt inside the
        // coroutine; strip is the coroutine residual (it absorbs the cheap per-cell UA
        // pre-filter query — the UA cost is the build, not the query).
        let kcyc = ph[2] as f64;
        let (pe_cyc, be_cyc, co_cyc) = (ph[3] as f64, ph[4] as f64, ph[5] as f64);
        let (fill_cyc, ua_cyc, verify_cyc) = (ph[8] as f64, ph[9] as f64, ph[10] as f64);
        let strip_cyc = (co_cyc - fill_cyc - ua_cyc - verify_cyc).max(0.0);
        let probe_warp = kcyc * probe_pass_share;
        let base_warp = kcyc * (1.0 - probe_pass_share);
        let tcyc = kcyc + pe_cyc + be_cyc + co_cyc; // everything attributed
        let env_cyc = (env_cycles as f64).max(tcyc); // envelope >= accounted (guard)
        let unacc = env_cyc - tcyc;

        // Calibrate tsc->wall over the shared span (us_per_cyc = wall_us / env_cyc), so the
        // rows sum exactly to the measured wall us/att. `us`/`pct` are of TOTAL runtime.
        let wall_us = dt.as_secs_f64() * 1e6;
        let us = |c: f64| wall_us * c / env_cyc / attempts;
        let pct = |c: f64| 100.0 * c / env_cyc;

        println!(
            "  warp_pass calls/att {:.1}  (util {:.3}, {:.2}/{lanes_const} lanes active per call)",
            uw[0] as f64 / attempts,
            uw[1] as f64 / (lanes_const as f64 * uw[0].max(1) as f64),
            uw[1] as f64 / uw[0].max(1) as f64,
        );
        println!(
            "  lane-passes/att: total {:.1}  probe {:.1} ({:.1}%)  baseline {:.1} ({:.1}%)",
            lane_passes / attempts,
            probe_lp / attempts,
            100.0 * probe_pass_share,
            base_lp / attempts,
            100.0 * (1.0 - probe_pass_share),
        );
        println!(
            "  warp_pass (kernel) timeshare {:.1}% of total  (probe {:.1}% / baseline {:.1}%)",
            pct(kcyc),
            pct(probe_warp),
            pct(base_warp),
        );
        println!("  phase breakdown (us/att, % of total wall):");
        let row =
            |name: &str, c: f64| println!("    {name:<19} {:>7.3} us  {:>5.1}%", us(c), pct(c));
        row("fill", fill_cyc);
        row("ua-build", ua_cyc);
        row("strip", strip_cyc);
        row("probe : warp_pass", probe_warp);
        row("probe : service", pe_cyc);
        row("baseline: warp_pass", base_warp);
        row("baseline: service", be_cyc);
        row("verify", verify_cyc);
        row("unaccounted", unacc);
        println!("    {:-<35}", "");
        row("total", env_cyc);
        println!(
            "    (probe  total {:>7.3} us  {:>5.1}%)   (baseline total {:>7.3} us  {:>5.1}%)",
            us(probe_warp + pe_cyc),
            pct(probe_warp + pe_cyc),
            us(base_warp + be_cyc),
            pct(base_warp + be_cyc),
        );
        println!(
            "  probe retirements: unique(keep) {} / non-unique(revert) {}  ({:.1}% revert by count)",
            ph[6],
            ph[7],
            100.0 * ph[7] as f64 / (ph[6] + ph[7]).max(1) as f64,
        );
    }

    // Two builds match iff they produce the same set of puzzles.
    println!("  fp: {combo_fp:#018x}");

    let s = &stats;
    let us_per_att = dt.as_secs_f64() * 1e6 / s.attempts.max(1) as f64;
    let att_per_puz = s.attempts as f64 / s.successes.max(1) as f64;
    // Average wall time for the W=8 warp to produce one puzzle.
    let s_per_puzzle = us_per_att * att_per_puz / 1e6;

    println!(
        "  {us_per_att:>7.2} us/att   yield {} / {} ({:.1} att/puzzle){}",
        s.successes,
        s.attempts,
        att_per_puz,
        if s.successes == 0 { "  [NONE in budget]" } else { "" },
    );
    if s.successes == 0 {
        println!("  s/puzzle: > {:.3}s  (no puzzle in {total} attempts; lower bound)", us_per_att * total as f64 / 1e6);
    } else {
        println!(
            "  s/puzzle: {s_per_puzzle:.3}s  (avg, W=8 warp){}",
            if s_per_puzzle > 1.0 { "  <-- > 1s target" } else { "" },
        );
    }
}
