//! `findpar-bench` -- the benchmarking sibling of `find`/`findpar`. Same `--force`/
//! `--toolbox` spec interface, but instead of stopping at a puzzle count it runs a FIXED
//! attempt budget (`--attempts` seeds, one attempt each) through the unified W=8 SIMT
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
//!       [--toolbox train|drill|full] [--attempts 800000] [--seed 1]
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
    let mut attempts = 800_000usize;
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
            "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
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
    let total = attempts;
    println!("findpar-bench[{label}] toolbox={toolbox}: {total} attempts");

    #[cfg(feature = "count")]
    {
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
        // --- profiler-style runtime attribution (counts + us/att, % of total wall) -----
        // Read it the way perf would: one attempt start to finish, every slice of wall
        // time named. The kernel `warp_pass_full` advances ALL active lanes in ONE shared
        // SIMD pass, so probe and baseline are not separate passes — they ride the same
        // pass — and the kernel's cost is split between them by LANE-PASS COUNT (each
        // tick's active lanes tallied by phase). The scalar tail is timed directly: engine
        // service per phase, the host coroutine resume (fill/ua/strip/verify), and the
        // per-tick warp-driver loop. The whole-run rdtsc envelope (`env_cyc`, same span as
        // the wall clock) turns the small leftover — pump bookkeeping, lane-service dispatch
        // micro-overhead, and rdtsc/instrumentation cost — into a measured `unaccounted`
        // slice, so every row is a true fraction of total runtime.
        let uw = generator_lab::solve::uwstat_snapshot();
        let lanes = generator_lab::probe::simt::LANES;
        let ph = generator_lab::generate::warp_host::phstat_snapshot();
        let attempts = stats.attempts.max(1) as f64;

        // Counts. A *lane-pass* is one active lane advanced by one shared kernel pass; a
        // probe (resp. baseline solve) spans many. probe/baseline lane-passes (ph[0]/ph[1])
        // partition the per-attempt SIMD step count by phase. Every gate the strip yields
        // runs one probe -- kept (unique, ph[6]) or reverted (non-unique, ph[7]); only a
        // unique keep flips the lane to a baseline solve, so baseline solves == unique keeps.
        let (probe_lp, base_lp) = (ph[0] as f64, ph[1] as f64);
        let lane_passes = (probe_lp + base_lp).max(1.0);
        let probe_pass_share = probe_lp / lane_passes;
        let avg_lanes = uw[1] as f64 / uw[0].max(1) as f64; // avg active lanes per kernel pass
        let (keeps, reverts) = (ph[6] as f64, ph[7] as f64);
        let probes = (keeps + reverts).max(1.0);

        // Each named cycle bucket. probe/baseline each = warp_pass (kernel lane-pass share)
        // + service (engine). fill/ua-build/verify are directly bracketed inside the
        // coroutine; tick-driver is the per-tick warp-loop plumbing around the kernel.
        let kcyc = ph[2] as f64;
        let (pe_cyc, be_cyc, co_cyc) = (ph[3] as f64, ph[4] as f64, ph[5] as f64);
        let (fill_cyc, ua_cyc, verify_cyc) = (ph[8] as f64, ph[9] as f64, ph[10] as f64);
        let tick_host_cyc = ph[11] as f64; // per-tick warp-driver plumbing (out of unaccounted)
        // strip() -- the per-cell strip with candidate elimination/propagation -- is the one
        // big piece of the coroutine remainder, so it is named. The rest (seed-setup, gate
        // I/O via export_r/revert/baseline, finalize, the cheap per-cell scans digit_at/
        // ua-caught/keep/reforce, and the coroutine resume machinery) each measured small and
        // stays lumped in the `unaccounted` child below. strip() carries ~0.5us of its own
        // per-call bracket overhead.
        let strip_core_cyc = ph[12] as f64;
        let co_unacc_cyc = (co_cyc - fill_cyc - ua_cyc - verify_cyc - strip_core_cyc).max(0.0);
        let probe_warp = kcyc * probe_pass_share;
        let base_warp = kcyc * (1.0 - probe_pass_share);
        let tcyc = kcyc + pe_cyc + be_cyc + co_cyc + tick_host_cyc; // everything attributed
        let env_cyc = (env_cycles as f64).max(tcyc); // envelope >= accounted (guard)
        let unacc = env_cyc - tcyc;

        // Calibrate tsc->wall over the shared span (us_per_cyc = wall_us / env_cyc), so the
        // rows sum exactly to the measured wall us/att. `us`/`pct` are of TOTAL runtime.
        let wall_us = dt.as_secs_f64() * 1e6;
        let us = |c: f64| wall_us * c / env_cyc / attempts;
        let pct = |c: f64| 100.0 * c / env_cyc;

        // -- warp fullness: how full is each shared kernel pass? (avg busy lanes / LANES).
        // This avg is the multiplier between calls and lane-passes, so the raw call count
        // adds nothing the lane-pass counts below don't already carry -- it is dropped.
        println!(
            "  warp: {avg_lanes:.2}/{lanes} lanes busy per pass (util {:.3})",
            avg_lanes / lanes as f64,
        );
        // -- per attempt: the counts one seed's strip walk drives. probes/baseline-solves
        // are the work units; lane-passes are the SIMD steps they cost (probe vs baseline
        // is the Q4 split). passes/probe and passes/baseline tie the two together.
        println!("  per attempt:");
        println!(
            "    probes          {:>6.1}  ({:.1}% revert)  {:.1} passes/probe",
            probes / attempts,
            100.0 * reverts / probes,
            probe_lp / probes,
        );
        println!(
            "    baseline solves {:>6.1}  (one per unique keep)  {:.1} passes/baseline",
            keeps / attempts,
            base_lp / keeps.max(1.0),
        );
        println!(
            "    lane-passes     {:>6.1}  probe {:.1} ({:.1}%)  baseline {:.1} ({:.1}%)",
            lane_passes / attempts,
            probe_lp / attempts,
            100.0 * probe_pass_share,
            base_lp / attempts,
            100.0 * (1.0 - probe_pass_share),
        );
        // -- time per attempt: every slice of wall named. The kernel's cost is split between
        // probe and baseline by their lane-pass share; service/host slices are timed directly.
        println!("  time per attempt (us, % of total wall):");
        // A 2-level profile tree: an indented row is a sub-breakdown of the row above it, so
        // each level's leftover is a child `unaccounted` attributed to its own parent.
        // `coroutine resume` is the slice measured directly (the bracket around `.resume()`);
        // its fill/ua-build/verify children are bracketed too, and its `unaccounted` child is
        // the remainder (strip walk + per-seed loop + resume machinery). The flush-left
        // `unaccounted` is the whole-run remainder (pump + lane-service dispatch + rdtsc).
        // The six flush-left rows sum to total; indented rows decompose their parent.
        let row =
            |name: &str, c: f64| println!("    {name:<21} {:>7.3} us  {:>5.1}%", us(c), pct(c));
        row("kernel (warp_pass)", kcyc);
        row("probe service", pe_cyc);
        row("baseline service", be_cyc);
        row("tick driver", tick_host_cyc);
        row("coroutine resume", co_cyc);
        row("  fill", fill_cyc);
        row("  ua-build", ua_cyc);
        row("  strip()", strip_core_cyc);
        row("  verify", verify_cyc);
        row("  unaccounted", co_unacc_cyc);
        row("unaccounted", unacc);
        println!("    {:-<37}", "");
        row("total", env_cyc);
        println!(
            "    probe total {:>7.3} us {:>5.1}%   baseline total {:>7.3} us {:>5.1}%   (kernel split by lane-pass share)",
            us(probe_warp + pe_cyc),
            pct(probe_warp + pe_cyc),
            us(base_warp + be_cyc),
            pct(base_warp + be_cyc),
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
