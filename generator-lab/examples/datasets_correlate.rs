//! Stage 1 of `docs/grader-external-calibration.md`: the **corrected scoreboard**. It reads the
//! normalised external datasets (`datasets/normalized/`), grades each group's puzzles spec-free
//! ([`grade_puzzle`], the G1 path), **buckets the covered puzzles by their inferred bottleneck
//! `key`** (incl. the trunk bucket, `key = None`), and reports, PER BUCKET:
//!
//!  - **n + coverage** — how many of the group's puzzles fell in this bucket (thin buckets are
//!    stated, never hidden).
//!  - **our `rating` vs the human label** — within-bucket rank correlation (weighted Spearman for
//!    the continuous groups, Kendall tau-b for the ordinal ones). The trunk bucket's rating is a
//!    flat `0`, so its correlation is undefined (`flat`) — finding §4.3.1, made visible here.
//!  - **Pelánek vs the human label on the SAME bucket (the bar)** — the Refutation sum and the
//!    Dependency metric, the same correlation type, over the same covered puzzles. This is what we
//!    measure ourselves against; we never tune to it.
//!
//! Per group the per-bucket correlations are aggregated by **n-weighted mean over buckets** (a
//! flat / undefined bucket contributes `0` — it has no ranking power — with its `n` still counted,
//! so the trunk drags our aggregate down exactly as it should). **Nothing is ever pooled across
//! buckets** (the one hard rule, §2.3): a per-technique rating only means "hard for its technique",
//! so an x-wing and a jellyfish rating are not comparable. The cross-technique "global" baseline of
//! the Stage-0 harness is **dropped** (the §2 scope decision: no global number).
//!
//! Pelánek is expensive (randomised rollouts), so its per-puzzle metrics are **cached** under
//! `--cache` keyed by the puzzle line (the seed is a deterministic function of the puzzle, not its
//! row position, so `--limit` never invalidates the cache). A re-run with the same `--runs`/`--k`/
//! `--model-seed` reloads instantly; the cache is invalidated only when those knobs change. Pass
//! `--no-pelanek` to skip the bar entirely for fast iteration on our own grader (Stages 2-4).
//!
//! Usage: cargo run --release -p generator-lab --example datasets_correlate -- \
//!          [--data DIR] [--jobs J=8] [--limit N] [--runs R=30] [--k K=25] \
//!          [--model-seed S=1] [--cache DIR] [--no-pelanek]

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use generator_lab::datasets::{Group, Row, load_group};
use generator_lab::grade::{PuzzleGrade, grade_puzzle};
use generator_lab::pelanek::{Opts, grade_puzzle as pelanek_grade};
use generator_lab::repr::DigitGrid;
use generator_lab::spec::kinds::NAMES;

// --- rank-correlation primitives ----------------------------------------------------------------

/// Fractional (tie-averaged) ranks of `xs` in `[0, 1)` — the mid-rank rule, for tie-robust
/// rank correlation.
fn frac_ranks(xs: &[f64]) -> Vec<f64> {
    let n = xs.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| xs[a].partial_cmp(&xs[b]).unwrap_or(Ordering::Equal));
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

/// Weighted Spearman rank correlation of `xs` vs `ys`: weighted Pearson over their fractional
/// ranks, weights `ws` (per-row confidence). With unit weights this is the plain Spearman.
/// `NAN` when either column is constant (no ranking power) — the caller treats that as a flat
/// `0` contribution in the aggregate.
fn weighted_spearman(xs: &[f64], ys: &[f64], ws: &[f64]) -> f64 {
    let (rx, ry) = (frac_ranks(xs), frac_ranks(ys));
    let wsum: f64 = ws.iter().sum();
    if wsum <= 0.0 {
        return f64::NAN;
    }
    let mx: f64 = rx.iter().zip(ws).map(|(a, w)| a * w).sum::<f64>() / wsum;
    let my: f64 = ry.iter().zip(ws).map(|(b, w)| b * w).sum::<f64>() / wsum;
    let mut cov = 0.0;
    let mut vx = 0.0;
    let mut vy = 0.0;
    for ((a, b), w) in rx.iter().zip(&ry).zip(ws) {
        cov += w * (a - mx) * (b - my);
        vx += w * (a - mx).powi(2);
        vy += w * (b - my).powi(2);
    }
    if vx < 1e-12 || vy < 1e-12 { f64::NAN } else { cov / (vx * vy).sqrt() }
}

/// Whether every element of `xs` is equal (a constant column carries no ranking information).
fn is_const(xs: &[f64]) -> bool {
    xs.first().is_none_or(|&first| xs.iter().all(|&x| x == first))
}

/// Kendall tau-b of `xs` vs `ys` (tie-aware, O(n^2) — fine on these bucket sizes). The right
/// measure for the ordinal groups, where the level index `ys` has big tie classes. `NAN` when
/// either column is constant (its variance — hence the correlation — is undefined), matching
/// [`weighted_spearman`]; the caller treats `NAN` as a flat `0` contribution in the aggregate.
fn kendall_tau_b(xs: &[f64], ys: &[f64]) -> f64 {
    if is_const(xs) || is_const(ys) {
        return f64::NAN;
    }
    let n = xs.len();
    let (mut conc, mut disc, mut tx, mut ty) = (0i64, 0i64, 0i64, 0i64);
    for i in 0..n {
        for j in (i + 1)..n {
            let dx = (xs[i] - xs[j]).partial_cmp(&0.0).unwrap_or(Ordering::Equal);
            let dy = (ys[i] - ys[j]).partial_cmp(&0.0).unwrap_or(Ordering::Equal);
            let xtie = dx == Ordering::Equal;
            let ytie = dy == Ordering::Equal;
            if xtie && ytie {
                // tied on both — counted in neither pairing total
            } else if xtie {
                tx += 1;
            } else if ytie {
                ty += 1;
            } else if dx == dy {
                conc += 1;
            } else {
                disc += 1;
            }
        }
    }
    let n0 = conc + disc + tx + ty; // pairs not tied on both
    let denom = (((n0 + tx) as f64) * ((n0 + ty) as f64)).sqrt();
    if denom <= 0.0 { f64::NAN } else { (conc - disc) as f64 / denom }
}

/// The two correlation regimes: continuous groups use weighted Spearman, ordinal groups Kendall
/// tau-b (§5 of the plan). `corr` dispatches a within-bucket column `xs` (a computed metric) vs the
/// human label `ys` with row weights `ws` (used only for the continuous, weighted Spearman).
#[derive(Clone, Copy)]
enum Metric {
    Continuous,
    Ordinal,
}

impl Metric {
    fn corr(self, xs: &[f64], ys: &[f64], ws: &[f64]) -> f64 {
        match self {
            Metric::Continuous => weighted_spearman(xs, ys, ws),
            Metric::Ordinal => kendall_tau_b(xs, ys),
        }
    }
}

/// An n-weighted accumulator over per-bucket correlations (§5: "aggregated per group, weighted by
/// bucket n, never pooled across techniques"). A flat / undefined bucket (`NAN`) contributes `0` —
/// it genuinely has no ranking power — with its `n` still counted, so the flat trunk bucket drags
/// the aggregate down rather than being silently dropped. Buckets too thin to correlate (`n < 2`)
/// are excluded from both numerator and denominator (and stated in the per-bucket lines).
#[derive(Default)]
struct Agg {
    num: f64,
    den: f64,
}

impl Agg {
    fn add(&mut self, rho: f64, n: usize) {
        if n >= 2 {
            self.num += if rho.is_finite() { rho } else { 0.0 } * n as f64;
            self.den += n as f64;
        }
    }
    fn get(&self) -> f64 {
        if self.den > 0.0 { self.num / self.den } else { f64::NAN }
    }
}

/// Format a correlation: `flat` for the undefined (constant-column) case, else signed 3-decimal.
fn fmt_rho(rho: f64) -> String {
    if rho.is_finite() { format!("{rho:+.3}") } else { " flat".to_string() }
}

// --- Pelánek metric cache -----------------------------------------------------------------------
//
// Pelánek is the expensive bar (randomised rollouts, 30 runs); we grade it once and cache. The
// cache is keyed by the puzzle line and the per-puzzle seed is a deterministic function of the
// puzzle (not its row position), so `--limit` subsampling never invalidates it and a full-run cache
// serves limited runs (and vice versa). Only `--runs`/`--k`/`--model-seed` invalidate the cache,
// detected by the header line.

/// A stable per-puzzle Pelánek model seed: FNV-1a of the puzzle line, mixed with the `--model-seed`
/// base. Position-independent, so the cache key is just the puzzle line.
fn puzzle_seed(puzzle: &str, base: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in puzzle.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^ base
}

/// The cache file's header — the knobs whose change must invalidate it.
fn cache_header(runs: usize, k: usize, base: u64) -> String {
    format!("# runs={runs} k={k} seed={base}")
}

/// Load a group's Pelánek cache: `puzzle -> (refutation_sum, dependency)`. Returns an empty map if
/// the file is missing or its header does not match the current knobs (a stale cache is discarded,
/// then overwritten on the next [`write_pel_cache`]).
fn load_pel_cache(path: &Path, runs: usize, k: usize, base: u64) -> HashMap<String, (f64, f64)> {
    let mut map = HashMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return map;
    };
    let mut lines = text.lines();
    match lines.next() {
        Some(h) if h.trim() == cache_header(runs, k, base) => {}
        _ => return map, // missing or stale header -> ignore
    }
    for line in lines {
        let mut c = line.split(',');
        if let (Some(p), Some(r), Some(d)) = (c.next(), c.next(), c.next()) {
            if let (Ok(r), Ok(d)) = (r.parse(), d.parse()) {
                map.insert(p.to_string(), (r, d));
            }
        }
    }
    map
}

/// Persist a group's Pelánek cache (sorted by puzzle for a stable file).
fn write_pel_cache(path: &Path, map: &HashMap<String, (f64, f64)>, runs: usize, k: usize, base: u64) {
    let mut rows: Vec<(&String, &(f64, f64))> = map.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let mut body = String::with_capacity(map.len() * 96 + 64);
    body.push_str(&cache_header(runs, k, base));
    body.push('\n');
    for (p, (r, d)) in rows {
        body.push_str(&format!("{p},{r},{d}\n"));
    }
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, body);
}

/// Grade `rows[i]` for every `i` in `need` with Pelánek, in parallel across `jobs` threads.
/// Returns `(puzzle, refutation_sum, dependency)` for those that solve uniquely.
fn compute_pelanek(
    rows: &[Row],
    need: &[usize],
    opts: &Opts,
    base: u64,
    jobs: usize,
) -> Vec<(String, f64, f64)> {
    let out: Vec<Mutex<Option<(String, f64, f64)>>> =
        (0..need.len()).map(|_| Mutex::new(None)).collect();
    let cursor = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let j = cursor.fetch_add(1, AtomicOrdering::Relaxed);
                    if j >= need.len() {
                        break;
                    }
                    let line = &rows[need[j]].puzzle;
                    if let Some(grid) = DigitGrid::parse(line) {
                        if let Some(m) = pelanek_grade(&grid, puzzle_seed(line, base), opts) {
                            *out[j].lock().unwrap() =
                                Some((line.clone(), m.refutation_sum, m.dependency));
                        }
                    }
                }
            });
        }
    });
    out.into_iter().filter_map(|m| m.into_inner().unwrap()).collect()
}

/// Ensure the Pelánek cache covers every covered (`ours[i].is_some()`) puzzle of the group, mining
/// only the shortfall, and return the metrics aligned to `rows` (`None` where our grader did not
/// cover it, or — rarely — Pelánek found no unique completion).
fn pelanek_for_group(
    rows: &[Row],
    ours: &[Option<PuzzleGrade>],
    cache_path: &Path,
    opts: &Opts,
    runs: usize,
    k: usize,
    base: u64,
    jobs: usize,
) -> Vec<Option<(f64, f64)>> {
    let mut map = load_pel_cache(cache_path, runs, k, base);
    let need: Vec<usize> = (0..rows.len())
        .filter(|&i| ours[i].is_some() && !map.contains_key(&rows[i].puzzle))
        .collect();
    if !need.is_empty() {
        for (p, r, d) in compute_pelanek(rows, &need, opts, base, jobs) {
            map.insert(p, (r, d));
        }
        write_pel_cache(cache_path, &map, runs, k, base);
    }
    (0..rows.len())
        .map(|i| if ours[i].is_some() { map.get(&rows[i].puzzle).copied() } else { None })
        .collect()
}

// --- our spec-free grader -----------------------------------------------------------------------

/// Grade every row of a group spec-free, in parallel across `jobs` threads. Returns the per-row
/// grade (None = the cold below-chains solve did not finish: ungradeable), aligned to `rows`.
fn grade_rows(rows: &[Row], jobs: usize) -> Vec<Option<PuzzleGrade>> {
    let out: Vec<Mutex<Option<PuzzleGrade>>> = (0..rows.len()).map(|_| Mutex::new(None)).collect();
    let cursor = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let i = cursor.fetch_add(1, AtomicOrdering::Relaxed);
                    if i >= rows.len() {
                        break;
                    }
                    if let Some(g) =
                        DigitGrid::parse(&rows[i].puzzle).and_then(|grid| grade_puzzle(&grid))
                    {
                        *out[i].lock().unwrap() = Some(g);
                    }
                }
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap()).collect()
}

// --- per-bucket scoreboard ----------------------------------------------------------------------

/// The `[0, 1]` rating distribution of a bucket: mean and how much it pins at the CDF ends
/// (`r == 0` / `r == 1` — the §4.3.2 distribution-shift read). The trunk bucket reads `mean 0.00,
/// lo 100%`; `armane/extreme` buckets pin high.
fn rating_diag(ratings: &[f64]) -> String {
    let n = ratings.len().max(1) as f64;
    let mean = ratings.iter().sum::<f64>() / n;
    let lo = ratings.iter().filter(|&&r| r <= 0.0).count();
    let hi = ratings.iter().filter(|&&r| r >= 1.0).count();
    format!("rating(mean {:.2} lo {:>3.0}% hi {:>3.0}%)", mean, 100.0 * lo as f64 / n, 100.0 * hi as f64 / n)
}

/// One group's per-technique-bucket scoreboard. Buckets the covered puzzles by inferred `key`
/// (trunk = `None`, sorted trunk-first then by kind), and for each bucket reports n, our `rating`
/// vs human, and Pelánek (refutation + dependency) vs human on the SAME bucket, then the
/// n-weighted aggregate across buckets. `pelanek[i]` is `None` when the bar is disabled
/// (`--no-pelanek`) or Pelánek found no unique completion.
fn report_group(group: &Group, ours: &[Option<PuzzleGrade>], pelanek: &[Option<(f64, f64)>], with_pel: bool) {
    let metric = if group.ordinal { Metric::Ordinal } else { Metric::Continuous };
    let n = group.rows.len();
    let covered: usize = ours.iter().filter(|o| o.is_some()).count();
    let pel_graded: usize = (0..n).filter(|&i| ours[i].is_some() && pelanek[i].is_some()).count();

    println!(
        "\n## {} [{}]  n={n}  covered={covered} ({:.0}%){}",
        group.id,
        if group.ordinal { "ordinal" } else { "continuous" },
        100.0 * covered as f64 / n as f64,
        if with_pel {
            format!("  pelanek graded={pel_graded}")
        } else {
            String::new()
        },
    );

    if group.ordinal {
        print_level_coverage(group, ours);
    }

    // Bucket the covered rows by inferred key (trunk = -1, else the kind index), sorted.
    let mut buckets: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, o) in ours.iter().enumerate() {
        if let Some(g) = o {
            buckets.entry(g.key.map_or(-1, |k| k as i64)).or_default().push(i);
        }
    }

    // Per-bucket lines + the three aggregates (ours, Pelánek refutation, Pelánek dependency).
    let (mut agg_our, mut agg_ref, mut agg_dep) = (Agg::default(), Agg::default(), Agg::default());
    for (&kb, idxs) in &buckets {
        let label = if kb < 0 { "trunk".to_string() } else { NAMES[kb as usize].to_string() };

        // Our rating vs human. When the bar is on, restrict to the SAME puzzles Pelánek graded so
        // the two correlations are apples-to-apples; otherwise use the full covered bucket.
        let in_cmp = |i: usize| !with_pel || pelanek[i].is_some();
        let cmp_idxs: Vec<usize> = idxs.iter().copied().filter(|&i| in_cmp(i)).collect();

        let our_x: Vec<f64> = cmp_idxs.iter().map(|&i| ours[i].unwrap().rating).collect();
        let labels: Vec<f64> = cmp_idxs.iter().map(|&i| group.rows[i].label_value).collect();
        let weights: Vec<f64> = cmp_idxs.iter().map(|&i| group.rows[i].weight).collect();
        let ratings_all: Vec<f64> = idxs.iter().map(|&i| ours[i].unwrap().rating).collect();

        // A bucket is only a *test* if it has >= 2 puzzles AND the human label varies within it.
        // A single-level (constant-label) bucket has no ground-truth order to match — restricted
        // range, §7 — so neither we nor Pelánek can be scored there: it is shown but excluded from
        // the aggregate. A bucket whose label varies but whose *metric* column is flat (the trunk's
        // rating, all `0`) IS a real failure to rank, so it counts as a `0` contribution.
        let n = cmp_idxs.len();
        let rankable = n >= 2 && !is_const(&labels);

        let our_rho = metric.corr(&our_x, &labels, &weights);
        if rankable {
            agg_our.add(our_rho, n);
        }

        let (ref_rho, dep_rho) = if with_pel {
            let refut: Vec<f64> = cmp_idxs.iter().map(|&i| pelanek[i].unwrap().0).collect();
            let depend: Vec<f64> = cmp_idxs.iter().map(|&i| pelanek[i].unwrap().1).collect();
            let rr = metric.corr(&refut, &labels, &weights);
            let dr = metric.corr(&depend, &labels, &weights);
            if rankable {
                agg_ref.add(rr, n);
                agg_dep.add(dr, n);
            }
            (rr, dr)
        } else {
            (f64::NAN, f64::NAN)
        };

        // A non-rankable bucket prints `n/a` (no testable order), not a misleading `flat`/`+0.000`.
        let cell = |rho: f64| if rankable { fmt_rho(rho) } else { "  n/a".to_string() };
        let tag = if n < 2 {
            " (thin)"
        } else if !rankable {
            " (1-level)"
        } else if n < 10 {
            " (thin)"
        } else {
            ""
        };
        let pel_str = if with_pel {
            format!("  pel:refut {} dep {}", cell(ref_rho), cell(dep_rho))
        } else {
            String::new()
        };
        println!(
            "   [{label:<12}] n={n:<4} ours {}{}   {}{tag}",
            cell(our_rho),
            pel_str,
            rating_diag(&ratings_all),
        );
    }

    // The aggregate scoreboard line — the board every later stage is judged on. `over=` is the
    // puzzle mass that entered the aggregate (rankable buckets only) vs the covered total, so the
    // excluded single-level / thin mass is visible rather than hidden.
    print!(
        "   AGG (n-weighted over buckets, flat->0; over {}/{} covered):  ours {}",
        agg_our.den as usize, covered, fmt_rho(agg_our.get()),
    );
    if with_pel {
        let bar = agg_ref.get().abs().max(agg_dep.get().abs());
        let gap = agg_our.get() - bar;
        print!("   pel:refut {} dep {}", fmt_rho(agg_ref.get()), fmt_rho(agg_dep.get()));
        print!("   | bar=max|pel| {:.3}  ours-bar {:+.3}", bar, gap);
    }
    println!();
}

/// For an ordinal group, the per-level coverage (overall the chains gap bites the top tier): how
/// many of each level's puzzles our cold solve finished. Levels are the distinct sorted
/// `label_value`s (ascending = harder).
fn print_level_coverage(group: &Group, ours: &[Option<PuzzleGrade>]) {
    let mut levels: Vec<i64> = group.rows.iter().map(|r| r.label_value.round() as i64).collect();
    levels.sort_unstable();
    levels.dedup();
    let mut tot = vec![0usize; levels.len()];
    let mut cov = vec![0usize; levels.len()];
    let idx = |v: f64| levels.iter().position(|&l| l == v.round() as i64).unwrap();
    for (i, row) in group.rows.iter().enumerate() {
        let li = idx(row.label_value);
        tot[li] += 1;
        if ours[i].is_some() {
            cov[li] += 1;
        }
    }
    print!("   per-level covered:");
    for li in 0..levels.len() {
        print!(" L{}={}/{}", li, cov[li], tot[li]);
    }
    println!();
}

fn arg_val(flag: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn arg_flag(flag: &str) -> bool {
    std::env::args().any(|a| a == flag)
}

fn main() {
    let jobs: usize =
        arg_val("--jobs").and_then(|s| s.parse().ok()).filter(|&j| j >= 1).unwrap_or(8);
    let limit: usize = arg_val("--limit").and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let runs: usize = arg_val("--runs").and_then(|s| s.parse().ok()).unwrap_or(30);
    let k: usize = arg_val("--k").and_then(|s| s.parse().ok()).unwrap_or(25);
    let model_seed: u64 = arg_val("--model-seed").and_then(|s| s.parse().ok()).unwrap_or(1);
    let with_pel = !arg_flag("--no-pelanek");
    let opts = Opts { refutation_runs: runs, dependency_runs: runs, dependency_k: k };

    let data_dir: PathBuf = arg_val("--data")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../datasets/normalized"));
    let cache_dir: PathBuf = arg_val("--cache")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/datasets-correlate-cache"));

    if !data_dir.is_dir() {
        eprintln!("no normalized datasets at {} (fetch + run scripts/datasets-normalize)", data_dir.display());
        std::process::exit(1);
    }

    // Load every <group>.csv (skip catalog.json), in a stable order.
    let mut files: Vec<PathBuf> = fs::read_dir(&data_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "csv").unwrap_or(false))
        .collect();
    files.sort();

    eprintln!(
        "datasets_correlate: {} groups, --jobs {jobs}, pelanek {} (runs={runs} k={k} seed={model_seed}), data {}",
        files.len(),
        if with_pel { "ON" } else { "OFF" },
        data_dir.display(),
    );
    println!("# Stage 1 — per-technique-bucket scoreboard (NO tuning). Rank WITHIN a bucket only;");
    println!("# aggregate is n-weighted over buckets, never pooled across techniques. Bar = Pelanek.");

    let t0 = std::time::Instant::now();
    for path in &files {
        let Some(mut group) = load_group(path) else {
            eprintln!("  skip (parse) {}", path.display());
            continue;
        };
        // `--limit N` subsamples N rows spread EVENLY across the file (the CSVs are grouped by
        // level, so a first-N prefix would be one constant-label block). Mirrors `pelanek --limit`.
        if limit < group.rows.len() {
            let len = group.rows.len();
            group.rows = (0..limit).map(|i| group.rows[i * len / limit].clone()).collect();
        }

        let ours = grade_rows(&group.rows, jobs);
        let pelanek = if with_pel {
            let cache_path = cache_dir.join(format!("{}.pelanek.csv", group.id.replace('/', "__")));
            pelanek_for_group(&group.rows, &ours, &cache_path, &opts, runs, k, model_seed, jobs)
        } else {
            vec![None; group.rows.len()]
        };
        report_group(&group, &ours, &pelanek, with_pel);
        eprintln!("  done {} ({:.1}s elapsed)", group.id, t0.elapsed().as_secs_f64());
    }
    eprintln!("datasets_correlate: all groups in {:.1}s", t0.elapsed().as_secs_f64());
}
