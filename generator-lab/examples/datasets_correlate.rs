//! Stage 0 of `docs/grader-external-calibration.md`: measure how badly the current grader
//! disagrees with **human** difficulty, with ZERO tuning. It reads the normalised external
//! datasets (`datasets/normalized/`), grades the solvable subset of each group spec-free
//! ([`grade_puzzle`], the G1 path) against the *existing baked tables*, and prints per group:
//!
//!  - **Coverage** — the fraction the cold below-chains solver can finish (the chains gap, a
//!    finding in itself). Reported per ordinal level too, since the hard tiers go near-zero.
//!  - **Correlation of the grader's cross-technique order against the human label** — weighted
//!    Spearman rho for the continuous groups (read against Pelanek 2014's r = 0.68 / 0.83
//!    yardstick), Kendall tau-b + monotone-mean-per-level + adjacent-level AUC for the ordinal
//!    groups.
//!  - **The G1 distribution-shift diagnostic** — the inferred-bottleneck mix and the rating
//!    distribution (how much it clamps at 0 / 1), exposing whether the isolated-mined tables fit
//!    the natural-puzzle distribution.
//!
//! The cross-technique number is the *un-fit* Stage-0 baseline: `global = DIFFICULTY[T] + rating_T`
//! — the project's own per-technique difficulty as `base_T` and `span_T = 1` (the C2 number of
//! §3-G2 before the data anchors `base_T`/`span_T` in Stage 2). It is exactly the current grader's
//! cross-technique opinion, which is what Stage 0 measures against humans.
//!
//! THE ONE HARD RULE (§2): rank is defined WITHIN a group only. Every number here is per group;
//! nothing is ever pooled across groups (that would re-introduce a fake absolute scale).
//!
//! Usage: cargo run --release -p generator-lab --example datasets_correlate -- \
//!          [--data DIR] [--jobs J=8]

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use generator_lab::grade::{PuzzleGrade, grade_puzzle};
use generator_lab::repr::DigitGrid;
use generator_lab::spec::kinds::{DIFFICULTY, NAMES};

/// One dataset row: the puzzle, the human label, and the per-row confidence weight. `level` is
/// the integer level index for an ordinal group (== `label_value`), unused for continuous.
struct Row {
    puzzle: String,
    label_value: f64,
    weight: f64,
}

/// One normalised group (`<group>.csv`): its id, whether the human label is continuous or an
/// ordinal level index, and the rows.
struct Group {
    id: String,
    ordinal: bool,
    rows: Vec<Row>,
}

/// Parse one `normalized/<group>.csv`. Group id = file stem with `__` -> `/`. The label kind is
/// read off the data: an ordinal group's `label_raw` is a level name (non-numeric), a continuous
/// group's is the numeric metric. Columns: `puzzle,label_value,label_raw,weight`.
fn load_group(path: &Path) -> Option<Group> {
    let text = fs::read_to_string(path).ok()?;
    let stem = path.file_stem()?.to_str()?;
    let id = stem.replacen("__", "/", 1);
    let mut lines = text.lines();
    lines.next()?; // header
    let mut rows = Vec::new();
    let mut ordinal = false;
    let mut first = true;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 4 {
            continue;
        }
        let puzzle = parts[0].trim().to_string();
        let label_value: f64 = parts[1].trim().parse().ok()?;
        let label_raw = parts[2].trim();
        let weight: f64 = parts[3].trim().parse().unwrap_or(1.0);
        if first {
            // Ordinal iff the raw label is a level *name*, not the numeric metric.
            ordinal = label_raw.parse::<f64>().is_err();
            first = false;
        }
        rows.push(Row { puzzle, label_value, weight });
    }
    Some(Group { id, ordinal, rows })
}

/// The Stage-0 cross-technique number for a graded puzzle: the project's own per-technique
/// [`DIFFICULTY`] as `base_T`, plus the within-technique rating. A trunk-only puzzle (no harder
/// bottleneck) scores `0` — the floor. This is the un-fit C2 baseline Stage 2 will anchor.
fn global_of(g: &PuzzleGrade) -> f64 {
    let base = g.key.map_or(0.0, |k| DIFFICULTY[k] as f64);
    base + g.rating
}

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

/// Kendall tau-b of `xs` vs `ys` (tie-aware, O(n^2) — fine on these group sizes). The right
/// measure for the ordinal groups, where the level index `ys` has big tie classes.
fn kendall_tau_b(xs: &[f64], ys: &[f64]) -> f64 {
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

/// Mann-Whitney AUC: P[a random `pos` value exceeds a random `neg` value], ties counted 0.5.
/// `> 0.5` means the higher level scores higher on average — the readable ordinal diagnostic.
fn auc(pos: &[f64], neg: &[f64]) -> f64 {
    if pos.is_empty() || neg.is_empty() {
        return f64::NAN;
    }
    let mut wins = 0.0;
    for &p in pos {
        for &q in neg {
            wins += match p.partial_cmp(&q) {
                Some(Ordering::Greater) => 1.0,
                Some(Ordering::Equal) => 0.5,
                _ => 0.0,
            };
        }
    }
    wins / (pos.len() * neg.len()) as f64
}

/// Grade every row of a group spec-free, in parallel across `jobs` threads. Returns the per-row
/// grade (None = the cold solve did not finish: ungradeable), aligned to `rows`.
fn grade_rows(rows: &[Row], jobs: usize) -> Vec<Option<PuzzleGrade>> {
    let out: Vec<Mutex<Option<PuzzleGrade>>> = (0..rows.len()).map(|_| Mutex::new(None)).collect();
    let cursor = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let i = cursor.fetch_add(1, AtomicOrdering::Relaxed);
                if i >= rows.len() {
                    break;
                }
                if let Some(g) = DigitGrid::parse(&rows[i].puzzle).and_then(|grid| grade_puzzle(&grid))
                {
                    *out[i].lock().unwrap() = Some(g);
                }
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap()).collect()
}

/// The G1 distribution-shift diagnostic for the covered subset: the rating mean and how much it
/// clamps at the CDF ends (`r==0` / `r==1` — natural puzzles falling outside the isolated-mined
/// range), plus the inferred-bottleneck mix (the top techniques the group reduces to).
fn shift_diag(graded: &[&PuzzleGrade]) -> String {
    let n = graded.len().max(1) as f64;
    let rmean = graded.iter().map(|g| g.rating).sum::<f64>() / n;
    let at0 = graded.iter().filter(|g| g.rating <= 0.0).count();
    let at1 = graded.iter().filter(|g| g.rating >= 1.0).count();
    let trunk = graded.iter().filter(|g| g.key.is_none()).count();
    let mut by_kind = [0usize; NAMES.len()];
    for g in graded {
        if let Some(k) = g.key {
            by_kind[k] += 1;
        }
    }
    let mut mix: Vec<(usize, usize)> =
        by_kind.iter().enumerate().filter(|&(_, &c)| c > 0).map(|(k, &c)| (k, c)).collect();
    mix.sort_by(|a, b| b.1.cmp(&a.1));
    let top: Vec<String> =
        mix.iter().take(4).map(|(k, c)| format!("{} {}", NAMES[*k], c)).collect();
    format!(
        "rating(mean {:.2} =0 {:>3.0}% =1 {:>3.0}% trunk {:>3.0}%) bottleneck[{}]",
        rmean,
        100.0 * at0 as f64 / n,
        100.0 * at1 as f64 / n,
        100.0 * trunk as f64 / n,
        top.join(", "),
    )
}

/// Report one continuous group: coverage + weighted Spearman of the global order vs the human
/// metric, over the covered subset.
fn report_continuous(g: &Group, graded: &[Option<PuzzleGrade>]) {
    let mut globals = Vec::new();
    let mut labels = Vec::new();
    let mut weights = Vec::new();
    let mut covered = Vec::new();
    for (row, pg) in g.rows.iter().zip(graded) {
        if let Some(pg) = pg {
            globals.push(global_of(pg));
            labels.push(row.label_value);
            weights.push(row.weight);
            covered.push(pg);
        }
    }
    let n = g.rows.len();
    let cov = covered.len();
    let unit = vec![1.0; weights.len()];
    let rho_w = weighted_spearman(&globals, &labels, &weights);
    let rho_u = weighted_spearman(&globals, &labels, &unit);
    println!("\n## {} [continuous]  n={n}  covered={cov} ({:.0}%)", g.id, 100.0 * cov as f64 / n as f64);
    println!("   weighted Spearman rho = {:+.3}   (unweighted {:+.3})   [Pelanek band r=0.68/0.83]", rho_w, rho_u);
    println!("   {}", shift_diag(&covered));
}

/// Report one ordinal group: coverage (overall + per level), Kendall tau-b of the global order vs
/// the level index, the mean-global-per-level monotonicity check, and the mean adjacent-level AUC.
fn report_ordinal(g: &Group, graded: &[Option<PuzzleGrade>]) {
    // Distinct sorted levels.
    let mut levels: Vec<i64> = g.rows.iter().map(|r| r.label_value.round() as i64).collect();
    levels.sort_unstable();
    levels.dedup();

    let mut globals = Vec::new();
    let mut lvl_f = Vec::new();
    let mut covered = Vec::new();
    // Per-level coverage + per-level global values.
    let mut tot_by_level = vec![0usize; levels.len()];
    let mut cov_by_level = vec![0usize; levels.len()];
    let mut globals_by_level: Vec<Vec<f64>> = vec![Vec::new(); levels.len()];
    let level_idx = |v: f64| levels.iter().position(|&l| l == v.round() as i64).unwrap();
    for (row, pg) in g.rows.iter().zip(graded) {
        let li = level_idx(row.label_value);
        tot_by_level[li] += 1;
        if let Some(pg) = pg {
            cov_by_level[li] += 1;
            let gl = global_of(pg);
            globals.push(gl);
            lvl_f.push(row.label_value);
            globals_by_level[li].push(gl);
            covered.push(pg);
        }
    }
    let n = g.rows.len();
    let cov = covered.len();
    let tau = kendall_tau_b(&globals, &lvl_f);

    println!("\n## {} [ordinal]  n={n}  covered={cov} ({:.0}%)", g.id, 100.0 * cov as f64 / n as f64);
    print!("   per-level covered:");
    for (li, lv) in levels.iter().enumerate() {
        let _ = lv;
        print!(" L{}={}/{}", li, cov_by_level[li], tot_by_level[li]);
    }
    println!();

    // Mean global per level + monotonicity (only over levels that have covered puzzles).
    let mut means: Vec<(usize, f64)> = Vec::new();
    for (li, gv) in globals_by_level.iter().enumerate() {
        if !gv.is_empty() {
            means.push((li, gv.iter().sum::<f64>() / gv.len() as f64));
        }
    }
    let mono = means.windows(2).all(|w| w[0].1 <= w[1].1 + 1e-9);
    let mean_str: Vec<String> = means.iter().map(|(li, m)| format!("L{}:{:.1}", li, m)).collect();

    // Mean adjacent-level AUC over levels that both have covered puzzles.
    let mut aucs = Vec::new();
    for w in 0..levels.len().saturating_sub(1) {
        if !globals_by_level[w].is_empty() && !globals_by_level[w + 1].is_empty() {
            aucs.push(auc(&globals_by_level[w + 1], &globals_by_level[w]));
        }
    }
    let auc_mean = if aucs.is_empty() { f64::NAN } else { aucs.iter().sum::<f64>() / aucs.len() as f64 };
    let auc_min = aucs.iter().cloned().fold(f64::INFINITY, f64::min);

    println!("   Kendall tau-b = {:+.3}   mean-global-per-level monotone={}   adj-AUC mean={:.2} min={:.2}",
        tau, if mono { "yes" } else { "NO" }, auc_mean, if auc_min.is_finite() { auc_min } else { f64::NAN });
    println!("   means [{}]", mean_str.join(" "));
    println!("   {}", shift_diag(&covered));
}

fn arg_val(flag: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn main() {
    let jobs: usize =
        arg_val("--jobs").and_then(|s| s.parse().ok()).filter(|&j| j >= 1).unwrap_or(8);
    let data_dir: PathBuf = arg_val("--data")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../datasets/normalized"));

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

    eprintln!("datasets_correlate: {} groups, --jobs {jobs}, data {}", files.len(), data_dir.display());
    println!("# Stage 0 — current grader vs human difficulty (NO tuning)");
    println!("# global = DIFFICULTY[T] + rating_T (un-fit C2 baseline). Rank WITHIN a group only.");

    let t0 = std::time::Instant::now();
    for path in &files {
        let Some(group) = load_group(path) else {
            eprintln!("  skip (parse) {}", path.display());
            continue;
        };
        let graded = grade_rows(&group.rows, jobs);
        if group.ordinal {
            report_ordinal(&group, &graded);
        } else {
            report_continuous(&group, &graded);
        }
    }
    eprintln!("datasets_correlate: graded all groups in {:.1}s", t0.elapsed().as_secs_f64());
}
