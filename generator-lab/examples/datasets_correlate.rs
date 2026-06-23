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
//! `--natural-remine` (§4.7) and `--trunk-average` (§4.8) run the two Stage-4 A/B passes after the
//! board (our grader only — pair with `--no-pelanek`): the natural NORM/CDF held-out re-mine, and the
//! trunk-frontier averaging over randomized fill orders vs the deterministic Stage-2 read.
//!
//! Usage: cargo run --release -p generator-lab --example datasets_correlate -- \
//!          [--data DIR] [--jobs J=8] [--limit N] [--runs R=30] [--k K=25] \
//!          [--model-seed S=1] [--cache DIR] [--no-pelanek] [--natural-remine] [--trunk-average]

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use generator_lab::datasets::{Group, Row, load_group};
use generator_lab::grade::{
    CDF_ANCHORS, GRANULAR_CDF, GRANULAR_NORM, PuzzleGrade, SIGN_DENSE_MIN, SigNorm, Signals, TechNorm,
    cdf_of, grade_puzzle, granular_score, rating_from_cdf, trunk_rating_runs,
};
use generator_lab::pelanek::{Opts, grade_puzzle as pelanek_grade};
use generator_lab::repr::DigitGrid;
use generator_lab::spec::kinds::{NAMES, NUM};

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

// --- Stage 4: the natural-puzzle re-mine (docs/grader-external-calibration.md §5) ----------------
//
// The spec-free path's baked tables ([`grade::NATURAL_NORM`]/[`grade::NATURAL_CDF`]) start identical
// to the *isolated* curriculum tables ([`GRANULAR_NORM`]/[`GRANULAR_CDF`]), which were mined on specs
// that force one technique. Natural dataset puzzles solved with the full toolbox land at a different
// signal/score distribution at the same inferred bottleneck (the §4.3.2 shift), so their scores
// overrun the isolated CDF's range and pin to a tie at `1.0` — all within-bucket resolution lost
// (e.g. `armane/extreme` swordfish, 63% clamped). Stage 4 re-mines a bucket's NORM(mean/scale)+CDF
// over the natural puzzles themselves, gated by a 2-fold held-out human correlation: a re-mine is
// baked only if it beats the isolated tables on a bucket half it was NOT fit on (criterion §6.4).

/// One technique kind's natural per-group bucket: the covered puzzles' [`Signals`], their human
/// labels and row weights, and the group's [`Metric`] — so the held-out validation uses the **same**
/// correlation the board is judged on (weighted Spearman for continuous, Kendall tau-b for ordinal).
struct KindGroup {
    metric: Metric,
    sigs: Vec<Signals>,
    labels: Vec<f64>,
    weights: Vec<f64>,
}

/// Bucket every group's covered, keyed (non-trunk) puzzles by inferred technique, returning per kind
/// its list of per-group [`KindGroup`]s. The trunk (`key == None`) is excluded — it has no
/// per-technique NORM/CDF (its sub-order is Stage 2's frontier rating, not a granular score).
fn natural_by_kind(graded: &[(Group, Vec<Option<PuzzleGrade>>)]) -> Vec<Vec<KindGroup>> {
    let mut per_kind: Vec<Vec<KindGroup>> = (0..NUM).map(|_| Vec::new()).collect();
    for (group, ours) in graded {
        let metric = if group.ordinal { Metric::Ordinal } else { Metric::Continuous };
        let mut by_kind: Vec<(Vec<Signals>, Vec<f64>, Vec<f64>)> =
            (0..NUM).map(|_| (Vec::new(), Vec::new(), Vec::new())).collect();
        for (i, o) in ours.iter().enumerate() {
            if let Some(g) = o {
                if let Some(k) = g.key {
                    by_kind[k].0.push(g.signals);
                    by_kind[k].1.push(group.rows[i].label_value);
                    by_kind[k].2.push(group.rows[i].weight);
                }
            }
        }
        for (k, (sigs, labels, weights)) in by_kind.into_iter().enumerate() {
            if !sigs.is_empty() {
                per_kind[k].push(KindGroup { metric, sigs, labels, weights });
            }
        }
    }
    per_kind
}

/// A candidate set of spec-free tables for a kind: a [`TechNorm`] and its granular-score CDF. `build`
/// (below) maps a training sample to one of these; [`heldout_rho`] then scores it on held-out halves.
type Tables = (TechNorm, [f64; CDF_ANCHORS]);

/// The three re-mine candidates for kind `k`, each as a `train_sample -> Tables` builder:
///  - **baseline** — the isolated `GRANULAR_*` row verbatim (ignores the sample); the bar to beat.
///  - **cdf** — keep the isolated NORM, re-mine only the CDF over the natural scores (un-clamp).
///  - **normcdf** — re-mean/scale the NORM on the natural sample too, then re-mine the CDF under it.
fn candidate(k: usize, which: &str) -> impl Fn(&[Signals]) -> Tables {
    move |train: &[Signals]| match which {
        "baseline" => (GRANULAR_NORM[k], GRANULAR_CDF[k]),
        "cdf" => {
            let norm = GRANULAR_NORM[k];
            let scores: Vec<f64> = train.iter().map(|s| granular_score(s, &norm)).collect();
            (norm, cdf_of(&scores))
        }
        _ /* normcdf */ => {
            let norm = GRANULAR_NORM[k].remeaned(train);
            let scores: Vec<f64> = train.iter().map(|s| granular_score(s, &norm)).collect();
            (norm, cdf_of(&scores))
        }
    }
}

/// The held-out human correlation of a candidate over a kind's per-group buckets: 2-fold CV. Each
/// group's bucket is split even/odd; in fold `f` the candidate trains on the pooled *other* halves
/// across groups and is scored on each group's held-out half-`f` (its own metric), pooled n-weighted
/// into one [`Agg`]. So a re-mine never sees the puzzles it is graded on. A test half whose label is
/// constant (no order to match) is skipped; a candidate whose rating is constant there (the baseline
/// clamping) scores `NaN` -> `0` with its n still counted, exactly as the board treats a flat bucket.
fn heldout_rho(groups: &[KindGroup], build: &dyn Fn(&[Signals]) -> Tables) -> Agg {
    let mut agg = Agg::default();
    for fold in 0..2usize {
        let train: Vec<Signals> = groups
            .iter()
            .flat_map(|g| g.sigs.iter().enumerate().filter(move |(i, _)| i % 2 != fold).map(|(_, s)| *s))
            .collect();
        if train.len() < 2 {
            continue;
        }
        let (norm, cdf) = build(&train);
        for g in groups {
            let idx: Vec<usize> = (0..g.sigs.len()).filter(|i| i % 2 == fold).collect();
            let y: Vec<f64> = idx.iter().map(|&i| g.labels[i]).collect();
            if idx.len() < 2 || is_const(&y) {
                continue; // no held-out order to match in this group/fold
            }
            let x: Vec<f64> =
                idx.iter().map(|&i| rating_from_cdf(granular_score(&g.sigs[i], &norm), &cdf)).collect();
            let w: Vec<f64> = idx.iter().map(|&i| g.weights[i]).collect();
            agg.add(g.metric.corr(&x, &y, &w), idx.len());
        }
    }
    agg
}

/// Minimum held-out human-rho gain for a re-mine to be **applied** over the isolated baseline (and
/// for `normcdf` to be preferred over the simpler `cdf`). Above the per-bucket CV noise; a smaller
/// gain is reported but not baked (keep the isolated row — fewer fitted numbers, less overfit).
const REMINE_MARGIN: f64 = 0.01;

/// Stage 4 driver: for each technique kind dense enough across the datasets (`>= SIGN_DENSE_MIN`
/// natural puzzles), report the 2-fold held-out human-rho of the isolated baseline vs the two
/// re-mine candidates, decide which (if any) to bake, then print the paste-ready `NATURAL_NORM` /
/// `NATURAL_CDF` arrays — accepted kinds re-mined over their FULL bucket (the validated method,
/// refit on all data), every other row the `GRANULAR_*` value verbatim.
fn natural_remine(graded: &[(Group, Vec<Option<PuzzleGrade>>)]) {
    let by_kind = natural_by_kind(graded);
    println!("\n# === STAGE 4 — natural-puzzle re-mine (2-fold held-out human-rho) ===");
    println!("# kind            n   baseline   cdf      normcdf   verdict");

    // The chosen builder per kind (None = keep the isolated GRANULAR_* row).
    let mut chosen: Vec<Option<&'static str>> = vec![None; NUM];
    for k in 0..NUM {
        let groups = &by_kind[k];
        let total: usize = groups.iter().map(|g| g.sigs.len()).sum();
        if total < SIGN_DENSE_MIN {
            if total > 0 {
                println!("# {:<14} n={:<4} thin (< {SIGN_DENSE_MIN}) -> keep isolated", NAMES[k], total);
            }
            continue;
        }
        let base = heldout_rho(groups, &candidate(k, "baseline")).get();
        let cdf = heldout_rho(groups, &candidate(k, "cdf")).get();
        let normcdf = heldout_rho(groups, &candidate(k, "normcdf")).get();

        // Prefer the simpler `cdf` unless `normcdf` clears it by the margin; apply only if the chosen
        // candidate clears the baseline by the margin.
        let (best, tag) =
            if normcdf - cdf >= REMINE_MARGIN { (normcdf, "normcdf") } else { (cdf, "cdf") };
        let verdict = if best - base >= REMINE_MARGIN {
            chosen[k] = Some(tag);
            format!("APPLY {tag} (+{:.3} held-out)", best - base)
        } else {
            format!("keep isolated (best {:+.3} <= base {:+.3})", best, base)
        };
        println!(
            "# {:<14} n={:<4} {:+.3}    {:+.3}   {:+.3}   {verdict}",
            NAMES[k], total, base, cdf, normcdf,
        );
    }

    print_natural_tables(&by_kind, &chosen);
    reweight_report(&by_kind, &chosen);
}

/// Print the full paste-ready `NATURAL_NORM` / `NATURAL_CDF` arrays. An accepted kind's row is the
/// chosen candidate re-mined over its whole bucket; every other row is the `GRANULAR_*` value
/// verbatim (so the spec-free path falls back to the isolated table there).
fn print_natural_tables(by_kind: &[Vec<KindGroup>], chosen: &[Option<&'static str>]) {
    let fmt_sig = |s: &SigNorm| {
        format!("SigNorm {{ mean: {:.3}, scale: {:.3}, sign: {:.1}, weight: {:.2} }}", s.mean, s.scale, s.sign, s.weight)
    };
    // kind -> the row's (norm, cdf): re-mined for accepted kinds, isolated otherwise.
    let tables: Vec<Tables> = (0..NUM)
        .map(|k| match chosen[k] {
            Some(which) => {
                let all: Vec<Signals> = by_kind[k].iter().flat_map(|g| g.sigs.iter().copied()).collect();
                candidate(k, which)(&all)
            }
            None => (GRANULAR_NORM[k], GRANULAR_CDF[k]),
        })
        .collect();

    println!("\n// === paste into grade.rs (Stage 4 natural re-mine) ===");
    println!("pub const NATURAL_NORM: [TechNorm; NUM] = [");
    for k in 0..NUM {
        let c = &tables[k].0;
        let tag = chosen[k].map_or("isolated", |w| w);
        println!(
            "    /* {:<14} */ TechNorm {{ open: {}, cascade: {}, depth: {}, elim: {}, alts: {}, \
             tight: {}, open_mean: {}, depth_tight: {}, transitions: {}, camo: {} }}, // {tag}",
            NAMES[k],
            fmt_sig(&c.open), fmt_sig(&c.cascade), fmt_sig(&c.depth), fmt_sig(&c.elim), fmt_sig(&c.alts),
            fmt_sig(&c.tight), fmt_sig(&c.open_mean), fmt_sig(&c.depth_tight), fmt_sig(&c.transitions), fmt_sig(&c.camo),
        );
    }
    println!("];");
    println!("pub const NATURAL_CDF: [[f64; CDF_ANCHORS]; NUM] = [");
    for k in 0..NUM {
        let anchors: Vec<String> = tables[k].1.iter().map(|x| format!("{x:.3}")).collect();
        let tag = chosen[k].map_or("isolated", |w| w);
        println!("    [{}], // {} ({tag})", anchors.join(", "), NAMES[k]);
    }
    println!("];");
}

// --- Stage 4 (optional): the per-technique re-weight (docs/grader-external-calibration.md §5) -----
//
// After the NORM/CDF re-mine, optionally re-fit each dense bucket's blend WEIGHTS against the human
// label. Strictly held-out: weights are fit on a training fold (coordinate ascent maximizing the
// n-weighted pooled per-group training human-rho) and judged on the other fold; a fit is reported as
// applicable only if its held-out human-rho beats the re-mine's on the same held-out halves. The doc
// warns this "may simply lack the data in most buckets — do not force it"; the gate enforces that.

const NSIG: usize = 10;

/// Number of independent 2-fold partitions the re-weight is cross-validated over. A single even/odd
/// 2-fold is high-variance on a thin bucket — a coordinate search can fit a weight vector that
/// generalises across *that one* split by luck. Requiring the held-out gain to survive every one of
/// several hash-shuffled partitions separates a real re-weight from split-luck (the doc's overfit risk).
const REWEIGHT_PARTITIONS: u64 = 6;

/// A balanced pseudo-random 2-fold assignment (`0` or `1`) of within-group index `i` under `salt` —
/// a cheap integer hash, so each salt is a different partition of the bucket. Deterministic (no RNG).
fn fold_hash(i: usize, salt: u64) -> usize {
    let mut h = (i as u64).wrapping_add(salt.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((h >> 31) & 1) as usize
}

/// A [`TechNorm`]'s 10 blend weights, in `raw_granular` order (the [`TechNorm::reweighted`] argument).
fn weights_of(n: &TechNorm) -> [f64; NSIG] {
    [
        n.open.weight, n.cascade.weight, n.depth.weight, n.elim.weight, n.alts.weight,
        n.tight.weight, n.open_mean.weight, n.depth_tight.weight, n.transitions.weight, n.camo.weight,
    ]
}

/// The n-weighted pooled per-group human-rho of `(norm, cdf)` over each group's indices where
/// `sel(i)` holds. The objective the weight fit climbs (on training indices) and the held-out gate
/// reads (on test indices); pooled per group, never across labels (§2.3).
fn pooled_rho(groups: &[KindGroup], sel: &dyn Fn(usize) -> bool, norm: &TechNorm, cdf: &[f64; CDF_ANCHORS]) -> f64 {
    let mut agg = Agg::default();
    for g in groups {
        let idx: Vec<usize> = (0..g.sigs.len()).filter(|&i| sel(i)).collect();
        let y: Vec<f64> = idx.iter().map(|&i| g.labels[i]).collect();
        if idx.len() < 2 || is_const(&y) {
            continue;
        }
        let x: Vec<f64> =
            idx.iter().map(|&i| rating_from_cdf(granular_score(&g.sigs[i], norm), cdf)).collect();
        let w: Vec<f64> = idx.iter().map(|&i| g.weights[i]).collect();
        agg.add(g.metric.corr(&x, &y, &w), idx.len());
    }
    agg.get()
}

/// Fit blend weights for a kind on the training fold (`sel` true), by bounded coordinate ascent over
/// the live signals: each live weight is tried at {0, ½, 2}× its baked value, the change kept if it
/// lifts the pooled training human-rho. Two passes; the search is deliberately small because the
/// per-group training halves are thin (a wider search just memorises them).
fn fit_weights(groups: &[KindGroup], sel: &dyn Fn(usize) -> bool, norm: &TechNorm, cdf: &[f64; CDF_ANCHORS]) -> [f64; NSIG] {
    let base = weights_of(norm);
    let mut best_w = base;
    let mut best = pooled_rho(groups, sel, &norm.reweighted(&best_w), cdf);
    for _pass in 0..2 {
        for j in 0..NSIG {
            if base[j] <= 0.0 {
                continue; // never light up a signal the branch left off
            }
            for &f in &[0.0f64, 0.5, 2.0] {
                let mut cand = best_w;
                cand[j] = base[j] * f;
                if cand.iter().sum::<f64>() <= 0.0 {
                    continue;
                }
                let rho = pooled_rho(groups, sel, &norm.reweighted(&cand), cdf);
                if rho.is_finite() && rho > best + 1e-9 {
                    best = rho;
                    best_w = cand;
                }
            }
        }
    }
    best_w
}

/// The §6.5 within-technique split-half rating stability floor a re-weight must clear (Stage 3's
/// gate, reused). A weight fit that collapses the blend onto one or two signals reorders puzzles
/// differently each resample — its two half-fits disagree — so this catches the thin-bucket overfit
/// the held-out human-rho alone can miss (a gain bought by discarding the within-technique structure).
const STABILITY_FLOOR: f64 = 0.90;

/// The split-half rating stability of the re-weight pipeline for kind `k` under re-mine `which`:
/// re-mine tables + fit weights on each half of the pooled natural bucket, rate the WHOLE pool with
/// each, and Spearman the two ratings. Near 1 iff the fitted weights reproduce across resamples (a
/// real, stable re-weight); low iff the fit overfits each half to a different degenerate vector.
fn reweight_stability(groups: &[KindGroup], k: usize, which: &str) -> f64 {
    let all: Vec<Signals> = groups.iter().flat_map(|g| g.sigs.iter().copied()).collect();
    let rate_half = |parity: usize| -> Vec<f64> {
        let train: Vec<Signals> = groups
            .iter()
            .flat_map(|g| g.sigs.iter().enumerate().filter(move |(i, _)| i % 2 == parity).map(|(_, s)| *s))
            .collect();
        let (norm, cdf) = candidate(k, which)(&train);
        let w = fit_weights(groups, &move |i| i % 2 == parity, &norm, &cdf);
        let rw = norm.reweighted(&w);
        all.iter().map(|s| rating_from_cdf(granular_score(s, &rw), &cdf)).collect()
    };
    let (a, b) = (rate_half(0), rate_half(1));
    let ones = vec![1.0; a.len()];
    weighted_spearman(&a, &b, &ones)
}

/// Stage 4 (optional): per dense kind, report the 2-fold held-out human-rho of the re-mine vs a
/// re-mine-plus-fitted-weights, layered on whichever re-mine the NORM/CDF gate chose. A fit is
/// applicable only if it BOTH clears the held-out human-rho margin AND holds the §6.5 split-half
/// stability floor (Stage 3's gate) — the second condition rejects the thin-bucket degenerate
/// collapses. Prints the verdict and, for any survivor, the fitted weight vector ready to bake.
fn reweight_report(by_kind: &[Vec<KindGroup>], chosen: &[Option<&'static str>]) {
    println!("\n# === STAGE 4 (optional) — per-technique re-weight (2-fold held-out) ===");
    println!("# kind            n   remine    +reweight  verdict");
    for k in 0..NUM {
        let groups = &by_kind[k];
        let total: usize = groups.iter().map(|g| g.sigs.len()).sum();
        if total < SIGN_DENSE_MIN {
            continue;
        }
        let which = chosen[k].unwrap_or("baseline");
        let build = candidate(k, which);

        // Repeated 2-fold CV: a held-out gain per partition. Report the mean and the WORST (min) — a
        // real re-weight is positive across partitions; split-luck swings.
        let mut gains: Vec<f64> = Vec::new();
        for salt in 0..REWEIGHT_PARTITIONS {
            let (mut base, mut rw) = (Agg::default(), Agg::default());
            for fold in 0..2usize {
                let train: Vec<Signals> = groups
                    .iter()
                    .flat_map(|g| g.sigs.iter().enumerate().filter(|(i, _)| fold_hash(*i, salt) != fold).map(|(_, s)| *s))
                    .collect();
                if train.len() < 2 {
                    continue;
                }
                let (norm, cdf) = build(&train);
                let on_train = move |i: usize| fold_hash(i, salt) != fold;
                let w = fit_weights(groups, &on_train, &norm, &cdf);
                let rwnorm = norm.reweighted(&w);
                for g in groups {
                    let idx: Vec<usize> = (0..g.sigs.len()).filter(|i| fold_hash(*i, salt) == fold).collect();
                    let y: Vec<f64> = idx.iter().map(|&i| g.labels[i]).collect();
                    if idx.len() < 2 || is_const(&y) {
                        continue;
                    }
                    let yw: Vec<f64> = idx.iter().map(|&i| g.weights[i]).collect();
                    let rate = |n: &TechNorm| -> Vec<f64> {
                        idx.iter().map(|&i| rating_from_cdf(granular_score(&g.sigs[i], n), &cdf)).collect()
                    };
                    base.add(g.metric.corr(&rate(&norm), &y, &yw), idx.len());
                    rw.add(g.metric.corr(&rate(&rwnorm), &y, &yw), idx.len());
                }
            }
            gains.push(rw.get() - base.get());
        }
        let mean_gain = gains.iter().sum::<f64>() / gains.len().max(1) as f64;
        let min_gain = gains.iter().cloned().fold(f64::INFINITY, f64::min);
        let stable = reweight_stability(groups, k, which);

        // The full-bucket fit (what a bake would use) — and its degeneracy check: a weight refinement
        // may rebalance the blend, but a fit that zeroes the branch's DOMINANT designed signal (the
        // largest baked weight) has *replaced* the within-technique rating with a single raw signal —
        // a Stage-3 orientation question, not a Stage-4 re-weight. Reject it (it discards the §6.5
        // within-technique structure the human anchor is meant to sit on top of).
        let all: Vec<Signals> = groups.iter().flat_map(|g| g.sigs.iter().copied()).collect();
        let (fnorm, fcdf) = build(&all);
        let fw = fit_weights(groups, &|_| true, &fnorm, &fcdf);
        let base_w = weights_of(&fnorm);
        let dom = (0..NSIG).max_by(|&a, &b| base_w[a].partial_cmp(&base_w[b]).unwrap()).unwrap();
        let degenerate = fw[dom] <= 0.0;

        // Apply only if the gain is robust across EVERY partition (min), split-half stable, AND not a
        // degenerate single-signal collapse.
        let apply = min_gain >= REMINE_MARGIN && stable >= STABILITY_FLOOR && !degenerate;
        let verdict = if apply {
            format!("APPLY reweight (mean +{mean_gain:.3}, min +{min_gain:.3}, stable {stable:.2})")
        } else if min_gain >= REMINE_MARGIN && degenerate {
            format!("REJECT (robust +{min_gain:.3} but drops the dominant '{}' signal — a Stage-3 orientation, not a re-weight)", SIG_NAMES[dom])
        } else if mean_gain >= REMINE_MARGIN {
            format!("REJECT (mean +{mean_gain:.3} but min {min_gain:+.3} < {REMINE_MARGIN:.2} — split-luck, not robust)")
        } else {
            format!("keep re-mine weights (mean gain {mean_gain:+.3})")
        };
        println!("# {:<14} n={:<4} mean{:+.3} min{:+.3} stable{:.2}  {verdict}", NAMES[k], total, mean_gain, min_gain, stable);
        if apply {
            println!("#   fitted weights: {fw:?}");
        }
    }
}

/// The granular signal names in `raw_granular`/weight order, for the re-weight degeneracy report.
const SIG_NAMES: [&str; NSIG] =
    ["open", "cascade", "depth", "elim", "alts", "tight", "open_mean", "depth_tight", "transitions", "camo"];

// --- Stage 4 (trunk): frontier averaging over fill orders (docs §4.5) ---------------------------
//
// Stage 2 reads the trunk frontier off ONE deterministic fill; Pelánek averages it over ~30
// randomized orders and our single read reaches only ~80-95% of its magnitude (§4.5 read 2). This
// pass recomputes the trunk bucket's rating with the frontier averaged over R runs and reports the
// trunk human-rho deterministic vs averaged (a few R), plus the resulting group aggregate — so the
// determinism-vs-averaging margin is measured before it is baked as `grade_puzzle`'s trunk default.
// Our grader only; no Pelánek. Run with `--trunk-average` (best with `--no-pelanek`).

/// The §4.5 number of randomized fill orders to A/B at (the last is `TRUNK_DEP_RUNS`).
const TRUNK_AVG_RS: [usize; 5] = [2, 4, 8, 16, 30];

/// Re-rate a group's `trunk` rows with the frontier averaged over `runs` fill orders, in parallel.
/// Aligned to `trunk` (a parse failure — never expected on covered rows — falls back to `0.0`).
fn trunk_avg_ratings(group: &Group, trunk: &[usize], runs: usize, jobs: usize) -> Vec<f64> {
    let out: Vec<Mutex<f64>> = (0..trunk.len()).map(|_| Mutex::new(0.0)).collect();
    let cursor = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let j = cursor.fetch_add(1, AtomicOrdering::Relaxed);
                    if j >= trunk.len() {
                        break;
                    }
                    if let Some(grid) = DigitGrid::parse(&group.rows[trunk[j]].puzzle) {
                        *out[j].lock().unwrap() = trunk_rating_runs(&grid, runs);
                    }
                }
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap()).collect()
}

/// The board's per-group aggregate (n-weighted over buckets, flat/single-level -> 0 with n counted,
/// `n < 2` excluded) of `ratings` (aligned to `ours`, covered entries only) vs the human label —
/// exactly the `--no-pelanek` aggregate, so swapping the trunk ratings shows the whole-group effect.
fn agg_with_ratings(group: &Group, ours: &[Option<PuzzleGrade>], ratings: &[f64]) -> f64 {
    let metric = if group.ordinal { Metric::Ordinal } else { Metric::Continuous };
    let mut buckets: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, o) in ours.iter().enumerate() {
        if let Some(g) = o {
            buckets.entry(g.key.map_or(-1, |k| k as i64)).or_default().push(i);
        }
    }
    let mut agg = Agg::default();
    for idxs in buckets.values() {
        let labels: Vec<f64> = idxs.iter().map(|&i| group.rows[i].label_value).collect();
        if idxs.len() < 2 || is_const(&labels) {
            continue;
        }
        let xs: Vec<f64> = idxs.iter().map(|&i| ratings[i]).collect();
        let ws: Vec<f64> = idxs.iter().map(|&i| group.rows[i].weight).collect();
        agg.add(metric.corr(&xs, &labels, &ws), idxs.len());
    }
    agg.get()
}

/// Stage-4 trunk-frontier-averaging report: per group, the trunk-bucket human-rho deterministic
/// (Stage 2) vs averaged at each `TRUNK_AVG_RS`, and the group aggregate det -> R=30.
fn trunk_average_report(graded: &[(Group, Vec<Option<PuzzleGrade>>)], jobs: usize) {
    println!("\n# === STAGE 4 (trunk) — frontier averaging over fill orders (§4.5) ===");
    let mut hdr = String::from("# group                      n_trunk   det   ");
    for r in TRUNK_AVG_RS {
        hdr.push_str(&format!(" R={r:<5}"));
    }
    hdr.push_str("   group AGG: det -> R30");
    println!("{hdr}");

    for (group, ours) in graded {
        let metric = if group.ordinal { Metric::Ordinal } else { Metric::Continuous };
        let trunk: Vec<usize> =
            (0..ours.len()).filter(|&i| ours[i].as_ref().is_some_and(|g| g.key.is_none())).collect();
        if trunk.len() < 2 {
            continue;
        }
        let labels: Vec<f64> = trunk.iter().map(|&i| group.rows[i].label_value).collect();
        let weights: Vec<f64> = trunk.iter().map(|&i| group.rows[i].weight).collect();
        if is_const(&labels) {
            continue; // single-level trunk (no within-bucket order) — not testable
        }

        // The full-group rating vector = `ours`, with the trunk rows overridden by `trunk_vals` —
        // so the swapped-trunk aggregate is apples-to-apples with the board's --no-pelanek AGG.
        let base_ratings = |trunk_vals: &[f64]| -> Vec<f64> {
            let mut r: Vec<f64> = ours.iter().map(|o| o.as_ref().map_or(0.0, |g| g.rating)).collect();
            for (j, &i) in trunk.iter().enumerate() {
                r[i] = trunk_vals[j];
            }
            r
        };

        // Deterministic baseline computed EXPLICITLY (`runs = 0`), so the A/B is independent of
        // whatever grade_puzzle's current trunk default is (after the bake `ours[i].rating` is the
        // average; this recovers the Stage-2 deterministic rating to compare against).
        let det_trunk = trunk_avg_ratings(group, &trunk, 0, jobs);
        let det_rho = metric.corr(&det_trunk, &labels, &weights);
        let det_agg = agg_with_ratings(group, ours, &base_ratings(&det_trunk));

        // Averaged trunk rho at each R, and (for R=30) the swapped whole-group aggregate.
        let mut line = format!("{:<26} n={:<5} {} ", group.id, trunk.len(), fmt_rho(det_rho));
        let mut r30_agg = det_agg;
        for r in TRUNK_AVG_RS {
            let avg = trunk_avg_ratings(group, &trunk, r, jobs);
            line.push_str(&format!(" {}", fmt_rho(metric.corr(&avg, &labels, &weights))));
            if r == 30 {
                r30_agg = agg_with_ratings(group, ours, &base_ratings(&avg));
            }
        }
        line.push_str(&format!("   {} -> {}", fmt_rho(det_agg), fmt_rho(r30_agg)));
        println!("{line}");
    }
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

    // `--natural-remine` runs the Stage-4 held-out re-mine (over our grader only) after the board.
    let remine = arg_flag("--natural-remine");
    // `--trunk-average` runs the Stage-4 trunk-frontier-averaging A/B (§4.5) after the board.
    let trunk_average = arg_flag("--trunk-average");

    let t0 = std::time::Instant::now();
    let mut graded: Vec<(Group, Vec<Option<PuzzleGrade>>)> = Vec::new();
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
        graded.push((group, ours));
    }
    eprintln!("datasets_correlate: all groups in {:.1}s", t0.elapsed().as_secs_f64());

    if remine {
        natural_remine(&graded);
    }
    if trunk_average {
        trunk_average_report(&graded, jobs);
    }
}
