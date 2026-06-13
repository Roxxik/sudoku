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
//!       [--toolbox train|drill|full] [--attempts 800000] [--seed 1] \
//!       [--techstats] [--techhist]
//!
//! NAME is any kind from `spec::kinds::NAMES` (e.g. `w-wing`, `naked-triple`,
//! `jellyfish`). COUNT defaults to 1. `--toolbox train` (default) allows the
//! union of each forced target's train-scope (Trunk + simpler-or-equal peers in
//! that target's branch); `--toolbox drill` concedes those simpler same-branch
//! peers instead of allowing them (the multi-force drill generalization — each
//! forced kind fires in the baseline trace rather than being fast-pathed by a
//! simpler peer); `--toolbox full` allows the entire 16-kind ladder (more
//! substitutes => rarer).
//!
//! `--techstats` (requires a `--features count` build) adds a per-technique census of
//! the **harder ladder** — every technique the warp drops to scalar when the cheap SIMD
//! closure (naked/hidden singles) stalls: locked candidates, the six subsets, the three
//! fishes, the three wings. For each it reports how many times the technique got its turn
//! in the ladder (`checked`) versus how many times it actually changed the board
//! (`fired`), with totals, per-attempt averages, and the fire rate. The closure's kernel
//! singles never enter the ladder, so they are absent by construction.
//!
//! `--techhist` adds, on top of the table, the distribution behind the `fired` average:
//! for each harder kind, a histogram of how many times it fired per **baseline solve**
//! (one per unique keep — the warp's unit of baseline servicing). It is read straight from
//! the warp's own per-solve traces, so it reports what the SIMT path actually did — NOT a
//! scalar re-solve, which would measure a different engine's bookkeeping (solver traces
//! are deliberately unpinned / reorderable) on a path this bench never exercises. All of
//! this is `count`-only, where timing is already instrumented rather than production-true.
//!
//! A SEPARATE `--features kernel_count` build adds detailed bookkeeping of the SIMD kernel
//! itself (`warp_pass_full` + `smear_v`), printed unconditionally (no flag). It is data-only
//! — popcounts in the innermost loop perturb timing heavily, so it is its own feature, never
//! `count`. Two views, both normalized to one active lane-pass ("one board in one pass"):
//! view 1 keeps the per-digit axis as averages (naked / row / box / column singles, net
//! placements, candidate eliminations, collisions); view 2 collapses digits into the
//! per-lane-pass distributions those averages hide (placed / naked / hidden row+box / row /
//! box / col / elim — the row+box vs col split being the kernel's two detection mechanisms).

use generator_lab::cli::{Toolbox, build_spec, parse_force, spec_label};
use generator_lab::generate::warp_host::{GateStream, Pumped};

// --- `--techstats` / `--techhist` support (the per-technique census is `count`-only) ---
#[cfg(feature = "count")]
use generator_lab::generate::warp_host::LSTATE_BINS;
#[cfg(feature = "count")]
use generator_lab::spec::kinds::{LC_POINTING, NAMES, NUM};

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
    // The per-technique census (`--techstats`) and its per-puzzle histogram (`--techhist`)
    // are read from the `count` counters; on a non-`count` build the flags only trigger a
    // note. `--techhist` implies the `--techstats` table (it is its detail view).
    let mut techstats = false;
    let mut techhist = false;

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
            "--techstats" => techstats = true,
            "--techhist" => techhist = true,
            _ => {}
        }
    }
    techstats |= techhist; // the histogram is the table's detail; never one without the other

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
        generator_lab::generate::warp_host::lstate_reset();
        generator_lab::generate::warp_host::svcgap_reset();
        generator_lab::solve::tchk_reset();
        generator_lab::solve::tfire_reset();
        generator_lab::solve::thist_reset();
    }
    #[cfg(feature = "kernel_count")]
    {
        generator_lab::solve::kstat_reset();
        generator_lab::solve::khist_reset();
        generator_lab::solve::krbtick_reset();
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

    // ----- harder-technique census (--techstats) + per-puzzle histogram (--techhist) -----
    #[cfg(feature = "count")]
    if techstats {
        let chk = generator_lab::solve::tchk_snapshot();
        let fire = generator_lab::solve::tfire_snapshot();
        let att = stats.attempts.max(1) as f64;
        // One row per harder technique that got a ladder turn. `checked == 0` rows are
        // omitted by construction: the kernel's naked/hidden singles never enter the
        // ladder, and lc-claiming is folded into the lc-pointing pass (so it never logs
        // its own turn). Index order IS ladder order for these kinds.
        println!("  harder techniques (checked = got a ladder turn, fired = changed the board):");
        println!(
            "    {:<14} {:>11} {:>11} {:>7}  {:>9} {:>9}",
            "technique", "checked", "fired", "fire%", "chk/att", "fire/att",
        );
        let mut lc_note = false;
        for k in 0..NUM {
            if chk[k] == 0 {
                continue;
            }
            lc_note |= k == LC_POINTING;
            let (c, f) = (chk[k] as f64, fire[k] as f64);
            println!(
                "    {:<14} {:>11} {:>11} {:>6.2}%  {:>9.4} {:>9.5}",
                NAMES[k],
                chk[k],
                fire[k],
                100.0 * f / c,
                c / att,
                f / att,
            );
        }
        if lc_note {
            println!("    (lc-pointing row is the fused pointing+claiming scalar pass)");
        }

        // -- lane states after each warp_pass: how the active lanes split, and how many are
        // STUCK (drop to scalar service: a probe branch or a baseline ladder step) at a
        // time. The per-pass means decompose the `warp:` line's active-lanes/pass; the
        // stuck distribution is the scalar-service pressure.
        let ls = generator_lab::generate::warp_host::lstate_snapshot();
        let passes: u64 = ls[4..4 + LSTATE_BINS].iter().sum(); // each pass: one stuck-hist entry
        let pf = passes.max(1) as f64;
        println!("  lane states after warp_pass (per-pass avg over {passes} passes):");
        println!(
            "    solved {:.3}   dead {:.3}   advanced {:.3}   stuck {:.3}   (lanes/pass)",
            ls[0] as f64 / pf,
            ls[1] as f64 / pf,
            ls[2] as f64 / pf,
            ls[3] as f64 / pf,
        );
        // Two lane-count histograms (0..=LANES lanes per pass), same shape: how many lanes
        // ADVANCED (kernel throughput) and how many were STUCK (scalar-service pressure) at
        // once. The mean is derived from the bins, so it cross-checks the [2]/[3] sums above.
        let lane_hist = |label: &str, bins: &[u64]| {
            let lanes: u64 = bins.iter().enumerate().map(|(n, &c)| n as u64 * c).sum();
            let top = bins.iter().rposition(|&c| c != 0).unwrap_or(0);
            println!(
                "  {label} lanes per pass: mean {:.3}, {:.1}% of passes have >=1:",
                lanes as f64 / pf,
                100.0 * (passes - bins[0]) as f64 / pf,
            );
            for (n, &cnt) in bins.iter().enumerate().take(top + 1) {
                println!("    {n}: {:>5.1}% ({cnt})", 100.0 * cnt as f64 / pf);
            }
        };
        lane_hist("advanced", &ls[4 + LSTATE_BINS..4 + 2 * LSTATE_BINS]);
        lane_hist("stuck", &ls[4..4 + LSTATE_BINS]);

        // -- per-lane service gap: how many warp passes a lane advances in the kernel before
        // it needs a scalar service (a run of 0 = stalled on its first pass after a load or a
        // prior service). One sample per service event; the last bin is the "or more"
        // overflow. The per-LANE dual of the per-PASS stuck histogram above.
        let svc = generator_lab::generate::warp_host::svcgap_snapshot();
        let runs: u64 = svc.iter().sum();
        let rf = runs.max(1) as f64;
        let cap = svc.len() - 1;
        let passes: u64 = svc.iter().enumerate().map(|(g, &c)| g as u64 * c).sum();
        let top = svc.iter().rposition(|&c| c != 0).unwrap_or(0);
        println!(
            "  warp passes a lane runs without servicing ({runs} runs, mean {:.2} passes/run):",
            passes as f64 / rf,
        );
        for (g, &cnt) in svc.iter().enumerate().take(top + 1) {
            let lbl = if g == cap { format!("{g}+") } else { g.to_string() };
            println!("    {lbl:>3}: {:>5.1}% ({cnt})", 100.0 * cnt as f64 / rf);
        }
    }

    #[cfg(feature = "count")]
    if techhist {
        // The distribution behind the table's fire/att average: for each harder kind, how
        // many times it fired per BASELINE SOLVE (one per unique keep -- the warp's unit of
        // baseline servicing), tallied straight from the warp's own traces (THIST). This is
        // what the SIMT path actually did -- not a scalar re-solve, which would measure a
        // different engine's (unpinned, reorderable) bookkeeping on a path the bench never
        // exercises. So it characterizes SIMT baseline servicing, not a puzzle property.
        let hist = generator_lab::solve::thist_snapshot();
        let cap = generator_lab::solve::THIST_CAP;
        // Total baseline solves = any one kind's bins summed (every retirement records all
        // kinds, bin 0 included); kind 0 (naked-single, never laddered) sums the same total.
        let total: u64 = hist[0..=cap].iter().sum();
        println!("  fires per baseline solve ({total} solves = unique keeps; harder kinds only):");
        if total == 0 {
            println!("    (no baseline solves in this budget)");
        }
        for k in 0..NUM {
            let base = k * (cap + 1);
            let bins = &hist[base..=base + cap];
            // Skip kinds that never fired (all mass in bin 0) -- incl. the kernel singles.
            if bins[1..].iter().all(|&c| c == 0) {
                continue;
            }
            let top = bins.iter().rposition(|&c| c != 0).unwrap_or(0); // highest non-empty bin
            // A one-line summary (mean fires/solve, and the share of solves that fired it
            // >= 1) precedes the bar breakdown.
            let sum_fires: u64 = bins.iter().enumerate().map(|(b, &c)| b as u64 * c).sum();
            let with_fire = total - bins[0];
            println!(
                "    {}: mean {:.3}/solve, {:.1}% of solves fire it",
                NAMES[k],
                sum_fires as f64 / total.max(1) as f64,
                100.0 * with_fire as f64 / total.max(1) as f64,
            );
            for (b, &cnt) in bins.iter().enumerate().take(top + 1) {
                let pct = 100.0 * cnt as f64 / total.max(1) as f64;
                let lbl = if b == cap { format!("{b}+") } else { b.to_string() };
                println!("      {lbl:>3}: {pct:>5.1}% ({cnt})");
            }
        }
    }

    #[cfg(not(feature = "count"))]
    if techstats {
        eprintln!("note: --techstats/--techhist need a `--features count` build; ignored");
    }

    // Detailed kernel bookkeeping (its own `kernel_count` feature; always emitted when built
    // with it, since that build exists only to read this -- timing there is meaningless). Two
    // separate views of the same kernel work, both normalized to "one board in one pass" (one
    // active lane-pass): View 1 keeps the digit axis as averages, View 2 collapses it into
    // per-lane-pass distributions (the spread the averages hide).
    #[cfg(feature = "kernel_count")]
    {
        use generator_lab::solve::kc;
        let k = generator_lab::solve::kstat_snapshot();
        let lp = k[kc::LANEPASS].max(1) as f64; // one active lane in one pass

        // -- View 1: per-DIGIT averages per lane-pass (the digit axis kept). naked vs
        // rowH+boxH+colH is the per-technique split; rowH/boxH/colH the per-unit-type one;
        // placed the net cells (<= naked+rowH+boxH+colH, the gap being multi-forced cells);
        // elim the smear's candidate clears; conflict the smear collisions.
        println!(
            "  kernel work / lane-pass [view 1: per digit] ({} passes, {} lane-passes, contradiction cells {:.4}/lp):",
            k[kc::CALLS],
            k[kc::LANEPASS],
            k[kc::DEAD] as f64 / lp,
        );
        let hdr = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str, g: &str, h: &str| {
            println!("    {a:>5} {b:>8} {c:>8} {d:>8} {e:>8} {f:>8} {g:>8} {h:>8}");
        };
        hdr("digit", "naked", "rowH", "boxH", "colH", "placed", "elim", "conflict");
        let cols = [kc::NAKED, kc::ROWH, kc::BOXH, kc::COLH, kc::PLACED, kc::PEERELIM, kc::CONFLICT];
        let mut tot = [0u64; 7];
        for d in 0..9 {
            let row: [u64; 7] = core::array::from_fn(|m| k[cols[m] + d]);
            for (t, &v) in tot.iter_mut().zip(row.iter()) {
                *t += v;
            }
            let s = |i: usize| format!("{:.4}", row[i] as f64 / lp);
            hdr(&(d + 1).to_string(), &s(0), &s(1), &s(2), &s(3), &s(4), &s(5), &s(6));
        }
        let s = |i: usize| format!("{:.4}", tot[i] as f64 / lp);
        hdr("all", &s(0), &s(1), &s(2), &s(3), &s(4), &s(5), &s(6));

        // -- View 2: per-lane-pass DISTRIBUTIONS (digits collapsed). For each metric, the
        // share of lane-passes whose across-digits total is n. Bins are contiguous 0..=max;
        // the means here equal View 1's `all` row (cross-check). `hidden` = row+box+col.
        let kh = generator_lab::solve::khist_snapshot();
        println!("  kernel work / lane-pass [view 2: distribution over lane-passes]:");
        // Mean is the EXACT KSTAT total / lane-passes (not bins x value), so the `KHIST_CAP`
        // overflow clamp doesn't bias it; it equals View 1's `all` row.
        let dist = |label: &str, base: usize, mean: f64| {
            let bins = &kh[base..=base + kc::KHIST_CAP];
            let total = bins.iter().sum::<u64>().max(1) as f64;
            let top = bins.iter().rposition(|&c| c != 0).unwrap_or(0);
            println!("    {label} (mean {mean:.3}):");
            for (n, &cnt) in bins.iter().enumerate().take(top + 1) {
                let lbl = if n == kc::KHIST_CAP { format!("{n}+") } else { n.to_string() };
                println!("      {lbl:>3}: {:>5.1}% ({cnt})", 100.0 * cnt as f64 / total);
            }
        };
        dist("placed", kc::KH_PLACED, tot[4] as f64 / lp);
        dist("naked", kc::KH_NAKED, tot[0] as f64 / lp);
        dist("hidden row+box", kc::KH_ROWBOX, (tot[1] + tot[2]) as f64 / lp);
        dist("hidden row", kc::KH_ROW, tot[1] as f64 / lp);
        dist("hidden box", kc::KH_BOX, tot[2] as f64 / lp);
        dist("hidden col", kc::KH_COL, tot[3] as f64 / lp);
        dist("elim", kc::KH_ELIM, tot[5] as f64 / lp);

        // Row/box load-bearing: lane-passes the full pass advances but a reduced naked-union-
        // col pass would stall on (only row/box hidden singles fired). The cost case for the
        // kernel's row/box detection path -- how often it is the sole source of progress.
        let rb_only = k[kc::RBONLY];
        let placed_any: u64 = kh[kc::KH_PLACED + 1..=kc::KH_PLACED + kc::KHIST_CAP].iter().sum();
        println!(
            "  row/box load-bearing: {rb_only} lane-passes ({:.2}% of placing, {:.2}% of all) advance via row/box only (naked u col would stall)",
            100.0 * rb_only as f64 / placed_any.max(1) as f64,
            100.0 * rb_only as f64 / lp,
        );
        // Same condition, per TICK: of the lanes sharing one pass, how many were row/box
        // load-bearing at once (bins sum to ticks, not lane-passes).
        let kt = generator_lab::solve::krbtick_snapshot();
        let ticks = kt.iter().sum::<u64>().max(1) as f64;
        let mean = kt.iter().enumerate().map(|(n, &c)| n as f64 * c as f64).sum::<f64>() / ticks;
        let top = kt.iter().rposition(|&c| c != 0).unwrap_or(0);
        println!("  row/box load-bearing lanes per tick (mean {mean:.3}):");
        for (n, &cnt) in kt.iter().enumerate().take(top + 1) {
            println!("    {n}: {:>5.1}% ({cnt})", 100.0 * cnt as f64 / ticks);
        }
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
