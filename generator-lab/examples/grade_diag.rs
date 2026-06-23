//! Grader tuning aid: mine a corpus of puzzles for the UI's campaign specs ONCE into a
//! resumable on-disk cache, then re-grade them instantly so the grader (`grade_one` /
//! `hardness_score` / `band_calibrated`) can be tuned without re-mining.
//!
//! Mining is the expensive part (some hard targets are multiple seconds per puzzle), so
//! puzzles are persisted per (mode, target) under a cache dir and NEVER re-derived: each
//! cache file carries a resume `frontier` seed, and a re-run only mines the shortfall to
//! reach `--count`. Mining races the SIMT warp (`GateStream`) where it gates soundly and
//! falls back to scalar `attempt` otherwise (mirrors `harvest`), and runs across specs on
//! up to `--jobs` threads (default 4). Grading then loads the cache and reports the band
//! distribution + raw signal ranges per spec.
//!
//! It reports, per node, the OLD coarse band split ([`band_calibrated`]/[`hardness_score`])
//! beside the NEW granular split ([`granular_score`], `docs/grader-granular-scoring.md` Tier A:
//! the continuous `open`/`cascade`/`depth` stall signals), plus a per-technique ACCEPTANCE
//! block (distinct p33/p66 cut, even-thirds tolerance, faithfulness vs the relative
//! [`grade_signals`] grader on a held-out half) so a change can be measured against the spec's
//! acceptance criteria without re-mining.
//!
//! With `--calibrate` it also prints the paste-ready granular calibration: per technique the
//! [`TechNorm`] literal (mean/scale/sign/weight per signal) and the `(p33, p66)` granular cut
//! points, both pooled over train+drill.
//!
//! Usage: cargo run --release -p generator-lab --example grade_diag -- \
//!          [--count N=300] [--jobs J=4] [--cache DIR] [--calibrate]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use generator_lab::generate::warp_host::{GateStream, Pumped};
use generator_lab::generate::{AttemptResult, attempt, baseline_fast_applicable};
use generator_lab::grade::{
    CDF_ANCHORS, SigNorm, Signals, TechNorm, Weights, band_coarse, band_of_rating, cdf_of,
    grade_one, grade_signals, granular_score, rating_from_cdf,
};
use generator_lab::repr::DigitGrid;
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{self, NAMES, NUM};

/// A campaign node: its mode (train/drill) and target kind. The spec is rebuilt on demand
/// (the isolated builders the UI uses), so only these two identify the cache file.
#[derive(Clone, Copy)]
struct Node {
    drill: bool,
    target: usize,
}

impl Node {
    fn spec(&self) -> Spec {
        if self.drill {
            Spec::drill_isolated(self.target)
        } else {
            Spec::train_isolated(self.target)
        }
    }
    fn label(&self) -> String {
        format!("{} {}", if self.drill { "drill" } else { "train" }, NAMES[self.target])
    }
    fn cache_file(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}_{}.txt", if self.drill { "drill" } else { "train" }, NAMES[self.target]))
    }
}

/// A spec's cached puzzles: the resume `frontier` (first un-scanned seed) and the
/// `(seed, 81-char line)` hits found so far.
struct Cache {
    frontier: u64,
    hits: Vec<(u64, String)>,
}

impl Cache {
    fn load(path: &Path) -> Cache {
        let mut frontier = 1u64;
        let mut hits = Vec::new();
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("# frontier ") {
                    frontier = rest.trim().parse().unwrap_or(1);
                } else if let Some((seed, puzzle)) = line.split_once(' ') {
                    if let Ok(seed) = seed.parse::<u64>() {
                        hits.push((seed, puzzle.to_string()));
                    }
                }
            }
        }
        Cache { frontier, hits }
    }

    fn write(&self, path: &Path) {
        let mut body = String::with_capacity(self.hits.len() * 90 + 32);
        body.push_str(&format!("# frontier {}\n", self.frontier));
        for (seed, line) in &self.hits {
            body.push_str(&format!("{seed} {line}\n"));
        }
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(path, body);
    }
}

/// Mine `node` until its cache holds at least `need` puzzles, persisting to `path`. Resumes
/// from the stored `frontier` and dedups by seed, so puzzles are never re-derived. Warp
/// where sound, scalar otherwise.
fn ensure_mined(node: Node, path: &Path, need: usize) -> Cache {
    let mut cache = Cache::load(path);
    if cache.hits.len() >= need {
        return cache;
    }
    let spec = node.spec();
    let mut seen: HashSet<u64> = cache.hits.iter().map(|&(s, _)| s).collect();
    let want = need - cache.hits.len();
    let mut got = 0usize;

    if baseline_fast_applicable(&spec) {
        let mut stream = GateStream::new(cache.frontier.., &spec);
        while got < want {
            match stream.pump(4096) {
                Pumped::Found(seed, p) => {
                    if seen.insert(seed) {
                        cache.hits.push((seed, p.puzzle.0.to_line()));
                        got += 1;
                    }
                }
                Pumped::StepCountReached => {}
                Pumped::NoMorePuzzles => break,
            }
        }
        cache.frontier += stream.stats().attempts as u64;
    } else {
        let mut seed = cache.frontier;
        let cap = seed + 50_000_000;
        while got < want && seed < cap {
            if let AttemptResult::Success(p) = attempt(&mut Rng::from_seed(seed), &spec) {
                if seen.insert(seed) {
                    cache.hits.push((seed, p.puzzle.0.to_line()));
                    got += 1;
                }
            }
            seed += 1;
        }
        cache.frontier = seed;
    }

    cache.hits.sort_by_key(|&(s, _)| s);
    cache.write(path);
    cache
}

/// The grade signals for up to `n` of a spec's cached puzzles (the UI's `grade_one` path).
fn signals_of_cache(node: Node, cache: &Cache, n: usize) -> Vec<Signals> {
    let spec = node.spec();
    cache
        .hits
        .iter()
        .take(n)
        .filter_map(|(_, line)| DigitGrid::parse(line))
        .map(|g| grade_one(&spec, &g).0)
        .collect()
}

/// One technique's granular calibration (Tier C): the per-signal [`TechNorm`] and the
/// per-technique granular-score CDF, both over the pooled train+drill sample. The CDF turns a raw
/// score into a continuous `[0, 1]` within-technique percentile ([`rating_of`]).
struct Calib {
    norm: TechNorm,
    cdf: [f64; CDF_ANCHORS],
}

/// Calibrate one technique `kind` from its pooled `sigs`: orient + scale the [`TechNorm`] against
/// the relative [`grade_signals`] reference (the absolute path stands in for the same ordering),
/// then build the granular-score CDF the rating reads against. The kind keys the per-technique
/// `camo` weight override inside [`TechNorm::calibrate`].
fn calibrate_target(kind: usize, sigs: &[Signals]) -> Calib {
    let rel = grade_signals(sigs, &Weights::default(), 3);
    let rel_scores: Vec<f64> = rel.iter().map(|g| g.score).collect();
    let norm = TechNorm::calibrate(kind, sigs, &rel_scores);
    let scores: Vec<f64> = sigs.iter().map(|s| granular_score(s, &norm)).collect();
    Calib { norm, cdf: cdf_of(&scores) }
}

/// One puzzle's continuous `[0, 1]` within-technique rating under a calibration.
fn rating_of(calib: &Calib, s: &Signals) -> f64 {
    rating_from_cdf(granular_score(s, &calib.norm), &calib.cdf)
}

/// Fractional (tie-averaged) ranks of `xs`, in `[0, 1)` — the same mid-rank rule the grader
/// uses, for a tie-robust correlation diagnostic.
fn frac_ranks(xs: &[f64]) -> Vec<f64> {
    let n = xs.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| xs[a].partial_cmp(&xs[b]).unwrap());
    let mut r = vec![0.0; n];
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && xs[order[j]] == xs[order[i]] {
            j += 1;
        }
        let p = (i as f64 + 0.5 * (j - i) as f64) / n as f64;
        for &k in &order[i..j] {
            r[k] = p;
        }
        i = j;
    }
    r
}

/// Spearman rank correlation of `xs` vs `ys` (Pearson on their fractional ranks). A cleaner
/// faithfulness read than band-agreement: it measures whether the two orderings agree, free of
/// the noise two independent 3-way cuts add at their boundaries.
fn spearman(xs: &[f64], ys: &[f64]) -> f64 {
    let (rx, ry) = (frac_ranks(xs), frac_ranks(ys));
    let n = rx.len() as f64;
    let (mx, my) = (rx.iter().sum::<f64>() / n, ry.iter().sum::<f64>() / n);
    let mut cov = 0.0;
    let mut vx = 0.0;
    let mut vy = 0.0;
    for (a, b) in rx.iter().zip(&ry) {
        cov += (a - mx) * (b - my);
        vx += (a - mx).powi(2);
        vy += (b - my).powi(2);
    }
    if vx < 1e-12 || vy < 1e-12 { 0.0 } else { cov / (vx * vy).sqrt() }
}

/// The UI band count we ship Tier C with (the user's 5-band ask).
const UI_BANDS: usize = 5;

/// Format one spec's OLD (coarse 3-band) vs NEW (granular Tier-C 5-band) split + signal ranges.
/// The granular split applies the target's pooled [`Calib`] (train and drill read against the same
/// pooled calibration, so drill reads spicier and train gentler within the technique — intended).
/// `bcount(avg .. multi ..%)` is the bottleneck multi-firing rate — the Tier-D driver: trajectory
/// features only diverge from the tightest-stall ones where the node fires the bottleneck > once.
fn report(node: Node, sigs: &[Signals], calib: &Calib) -> String {
    let spec = node.spec();
    let label = node.label();
    if sigs.is_empty() {
        return format!("{label:<28} no puzzles");
    }
    let count = sigs.len();
    let pct = |c: usize| 100.0 * c as f64 / count as f64;
    let avg_by = |f: fn(&Signals) -> u32| sigs.iter().map(|s| f(s) as f64).sum::<f64>() / count as f64;

    let mut old = [0usize; 3];
    let mut new = [0usize; UI_BANDS];
    for s in sigs {
        old[band_coarse(&spec, s).min(2)] += 1;
        new[band_of_rating(rating_of(calib, s), UI_BANDS)] += 1;
    }
    let scarce_vals: Vec<f64> =
        sigs.iter().filter(|s| s.scarcity != u32::MAX).map(|s| s.scarcity as f64).collect();
    let scarce_avg = if scarce_vals.is_empty() {
        f64::NAN
    } else {
        scarce_vals.iter().sum::<f64>() / scarce_vals.len() as f64
    };
    let new_str: Vec<String> = new.iter().map(|&c| format!("{:>3.0}", pct(c))).collect();
    let multi = 100.0 * sigs.iter().filter(|s| s.bottleneck_count > 1).count() as f64 / count as f64;
    format!(
        "{label:<24} n={count:<4} OLD3 g{:>3.0} m{:>3.0} s{:>3.0} | NEW5 [{}] | \
         bcount(avg {:.2} multi {:>3.0}%) open(avg {:.0}) casc(avg {:.1}) depth(avg {:.0}) scarce(avg {:.1}) alts(avg {:.1}) camo(avg {:.1})",
        pct(old[0]), pct(old[1]), pct(old[2]),
        new_str.join(" "),
        avg_by(|s| s.bottleneck_count), multi,
        avg_by(|s| s.open), avg_by(|s| s.cascade), avg_by(|s| s.depth), scarce_avg,
        avg_by(|s| s.alts), avg_by(|s| s.camo),
    )
}

/// One technique's ACCEPTANCE line: the Tier-C resolution + faithfulness checks
/// (`docs/grader-continuous-scoring.md` §2). `sigs` is the pooled train+drill sample.
/// - `levels` = number of non-empty `1/R` rating buckets (effective resolution, target ≥ 10);
/// - `even5` = each of the 5 UI bands within 20% ± 8;
/// - `stable` = split-half Spearman of the rating against itself (A-calibrated vs B-calibrated
///   on the shared pool), the granularity analogue of "distinct cut" (target ≥ 0.90);
/// - `rho` = Spearman of the granular score vs the relative grader (faithfulness, target ≥ 0.85).
fn acceptance(t: usize, sigs: &[Signals]) -> String {
    const R: usize = 10;
    let full = calibrate_target(t, sigs);
    let n = sigs.len();
    let ratings: Vec<f64> = sigs.iter().map(|s| rating_of(&full, s)).collect();

    // Effective resolution: how many 1/R rating buckets are populated.
    let mut buckets = [false; R];
    for &r in &ratings {
        buckets[band_of_rating(r, R)] = true;
    }
    let levels = buckets.iter().filter(|&&b| b).count();

    // 5-band evenness.
    let mut bands = [0usize; UI_BANDS];
    for &r in &ratings {
        bands[band_of_rating(r, UI_BANDS)] += 1;
    }
    let pct = |c: usize| 100.0 * c as f64 / n.max(1) as f64;
    let even5 = (0..UI_BANDS).all(|b| (pct(bands[b]) - 100.0 / UI_BANDS as f64).abs() <= 8.0);

    // Rank stability: calibrate on each half, rate the WHOLE pool with each, correlate the two
    // ratings. A reproducible ordering (not sampling noise) keeps this near 1.
    let a: Vec<Signals> = sigs.iter().step_by(2).copied().collect();
    let b: Vec<Signals> = sigs.iter().skip(1).step_by(2).copied().collect();
    let stable = if a.len() >= 10 && b.len() >= 10 {
        let (ca, cb) = (calibrate_target(t, &a), calibrate_target(t, &b));
        let ra: Vec<f64> = sigs.iter().map(|s| rating_of(&ca, s)).collect();
        let rb: Vec<f64> = sigs.iter().map(|s| rating_of(&cb, s)).collect();
        spearman(&ra, &rb)
    } else {
        f64::NAN
    };

    // Faithfulness: Spearman of the granular score vs the relative grader over the pool.
    let gran: Vec<f64> = sigs.iter().map(|s| granular_score(s, &full.norm)).collect();
    let rel_all = grade_signals(sigs, &Weights::default(), 3);
    let rho = spearman(&gran, &rel_all.iter().map(|g| g.score).collect::<Vec<_>>());

    let pass = levels >= R && even5 && stable >= 0.90 && rho >= 0.85;
    let band_str: Vec<String> = bands.iter().map(|&c| format!("{:>2.0}", pct(c))).collect();
    format!(
        "{:<14} levels={:>2}/{} [{}] even5={} stable={:+.2} rho={:+.2} {}",
        NAMES[t],
        levels,
        R,
        band_str.join(" "),
        if even5 { "ok " } else { "BAD" },
        stable,
        rho,
        if pass { "PASS" } else { "----" },
    )
}

/// Print the paste-ready Tier-C calibration: the full `GRANULAR_NORM` (per-technique [`TechNorm`])
/// and `GRANULAR_CDF` (per-technique granular-score percentile breakpoints) arrays, both indexed
/// by kind over all `NUM` rows (trunk + any uncalibrated kind = an unused placeholder), so the
/// output drops verbatim into `grade.rs`. Pooled train+drill.
fn calibrate(targets: &[usize], pooled: &[Vec<Signals>]) {
    let fmt_sig = |s: &SigNorm| {
        format!(
            "SigNorm {{ mean: {:.3}, scale: {:.3}, sign: {:.1}, weight: {:.2} }}",
            s.mean, s.scale, s.sign, s.weight
        )
    };
    // kind -> its calibration (only the harder targets are calibrated).
    let mut by_kind: Vec<Option<Calib>> = (0..NUM).map(|_| None).collect();
    for (i, &t) in targets.iter().enumerate() {
        if !pooled[i].is_empty() {
            by_kind[t] = Some(calibrate_target(t, &pooled[i]));
        }
    }
    let placeholder = "SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }";

    println!("=== CALIBRATION (paste into grade.rs; pooled train+drill) ===");
    println!("pub const GRANULAR_NORM: [TechNorm; NUM] = [");
    for k in 0..NUM {
        match &by_kind[k] {
            Some(c) => println!(
                "    /* {:<14} */ TechNorm {{ open: {}, cascade: {}, depth: {}, elim: {}, alts: {}, \
                 tight: {}, open_mean: {}, depth_tight: {}, transitions: {}, camo: {} }},",
                NAMES[k],
                fmt_sig(&c.norm.open),
                fmt_sig(&c.norm.cascade),
                fmt_sig(&c.norm.depth),
                fmt_sig(&c.norm.elim),
                fmt_sig(&c.norm.alts),
                fmt_sig(&c.norm.tight),
                fmt_sig(&c.norm.open_mean),
                fmt_sig(&c.norm.depth_tight),
                fmt_sig(&c.norm.transitions),
                fmt_sig(&c.norm.camo),
            ),
            None => println!(
                "    /* {:<14} */ TechNorm {{ open: {p}, cascade: {p}, depth: {p}, elim: {p}, alts: {p}, \
                 tight: {p}, open_mean: {p}, depth_tight: {p}, transitions: {p}, camo: {p} }},",
                NAMES[k],
                p = placeholder,
            ),
        }
    }
    println!("];");
    println!("pub const GRANULAR_CDF: [[f64; CDF_ANCHORS]; NUM] = [");
    for k in 0..NUM {
        match &by_kind[k] {
            Some(c) => {
                let anchors: Vec<String> = c.cdf.iter().map(|x| format!("{x:.3}")).collect();
                println!("    [{}], // {}", anchors.join(", "), NAMES[k]);
            }
            None => println!("    [0.0; CDF_ANCHORS], // {} (trunk/unused)", NAMES[k]),
        }
    }
    println!("];");
}

fn arg_val(flag: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn arg_flag(flag: &str) -> bool {
    std::env::args().any(|a| a == flag)
}

fn main() {
    let n: usize = arg_val("--count").and_then(|s| s.parse().ok()).unwrap_or(300);
    let jobs: usize = arg_val("--jobs").and_then(|s| s.parse().ok()).filter(|&j| j >= 1).unwrap_or(4);
    let cache_dir: PathBuf = arg_val("--cache")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/grade-cache"));

    // The campaign nodes: every target past the trunk, in train and drill.
    let targets: &[usize] = &[
        kinds::NAKED_PAIR, kinds::HIDDEN_PAIR, kinds::NAKED_TRIPLE, kinds::HIDDEN_TRIPLE,
        kinds::NAKED_QUAD, kinds::HIDDEN_QUAD, kinds::X_WING, kinds::SWORDFISH,
        kinds::JELLYFISH, kinds::XY_WING, kinds::XYZ_WING, kinds::W_WING,
    ];
    let mut nodes: Vec<Node> = Vec::new();
    for &t in targets {
        nodes.push(Node { drill: false, target: t });
    }
    for &t in targets {
        nodes.push(Node { drill: true, target: t });
    }

    eprintln!(
        "grade_diag: {} specs, --count {n}, --jobs {jobs}, cache {}",
        nodes.len(),
        cache_dir.display()
    );

    // Phase 1: ensure each spec's cache holds >= n puzzles (mining the shortfall in parallel
    // across <= jobs threads; a full spec just loads), then compute its grade signals.
    let results: Vec<Mutex<Vec<Signals>>> = (0..nodes.len()).map(|_| Mutex::new(Vec::new())).collect();
    let next = AtomicUsize::new(0);
    let t0 = std::time::Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= nodes.len() {
                    break;
                }
                let node = nodes[i];
                let cache = ensure_mined(node, &node.cache_file(&cache_dir), n);
                let sigs = signals_of_cache(node, &cache, n);
                eprintln!("  done {} ({} cached)", node.label(), cache.hits.len());
                *results[i].lock().unwrap() = sigs;
            });
        }
    });
    eprintln!("grade_diag: mined/graded in {:.1}s", t0.elapsed().as_secs_f64());

    // Lift the per-node signals out of the mutexes, then pool train+drill per target and
    // calibrate each technique's granular norm + thirds cut over that pool.
    let node_sigs: Vec<Vec<Signals>> = results.iter().map(|m| m.lock().unwrap().clone()).collect();
    let pooled: Vec<Vec<Signals>> = targets
        .iter()
        .enumerate()
        .map(|(ti, _)| {
            let mut v = node_sigs[ti].clone(); // train block
            v.extend(node_sigs[targets.len() + ti].iter().copied()); // drill block
            v
        })
        .collect();
    let calibs: Vec<Calib> =
        targets.iter().enumerate().map(|(ti, &t)| calibrate_target(t, &pooled[ti])).collect();
    // target kind -> its index in `targets`, so a node can find its pooled calibration.
    let calib_of = |target: usize| targets.iter().position(|&t| t == target).unwrap();

    // Phase 2: per-spec OLD vs NEW band distribution (train block, then drill block).
    println!("=== TRAIN (isolated) — OLD = coarse hardness_score | NEW = granular ===");
    for (i, nd) in nodes.iter().enumerate() {
        if !nd.drill {
            println!("{}", report(*nd, &node_sigs[i], &calibs[calib_of(nd.target)]));
        }
    }
    println!("=== DRILL (isolated) ===");
    for (i, nd) in nodes.iter().enumerate() {
        if nd.drill {
            println!("{}", report(*nd, &node_sigs[i], &calibs[calib_of(nd.target)]));
        }
    }

    // Acceptance (docs/grader-granular-scoring.md §2): per technique, pooled train+drill.
    println!("=== ACCEPTANCE (granular, pooled train+drill) ===");
    for (ti, &t) in targets.iter().enumerate() {
        if !pooled[ti].is_empty() {
            println!("{}", acceptance(t, &pooled[ti]));
        }
    }

    if arg_flag("--calibrate") {
        calibrate(targets, &pooled);
    }
}
