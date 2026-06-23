//! Compute the Pelánek (2014) difficulty metrics — Refutation sum and Dependency
//! — for either freshly generated puzzles or an external human-difficulty dataset.
//!
//! Two modes:
//!
//! - **generate** (default): the same `--force`/`--toolbox`/`--seed`/`--count`/
//!   `--max` contract as `examples/find` and `examples/grade`. Generates a node's
//!   yield and prints each puzzle's two metrics, hardest (highest refutation sum)
//!   first.
//!
//!     cargo run --release -p generator-lab --example pelanek -- \
//!         --force NAKED_PAIR --count 20 [--runs 30] [--k 25]
//!
//! - **dataset** (`--dataset PATH`): grade every puzzle in a normalized dataset CSV
//!   (`datasets/normalized/*.csv`: `puzzle,label_value,label_raw,weight`, higher
//!   `label_value` = harder) and report the Pearson and Spearman correlation of each
//!   metric against the human label — the faithfulness check against the paper's
//!   headline r ≈ 0.68 (refutation) / 0.67 (dependency). Dependency is inverse, so
//!   its correlation is expected to be negative. `--limit N` subsamples N puzzles
//!   spread evenly across the whole file (the CSVs are grouped by level, so a first-N
//!   prefix would be one constant-label block).
//!
//!     cargo run --release -p generator-lab --example pelanek -- \
//!         --dataset ../../datasets/normalized/synnwang__solve_time.csv [--limit 300] [--runs 30]
//!
//! Knobs shared by both modes: `--runs N` (model runs averaged, paper = 30), `--k N`
//! (Dependency step window, paper ≈ 25), `--model-seed S` (the rollout RNG seed).

use generator_lab::cli::FindArgs;
use generator_lab::generate::{AttemptResult, attempt};
use generator_lab::pelanek::{Opts, grade_puzzle};
use generator_lab::repr::DigitGrid;
use generator_lab::rng::Rng;

fn main() {
    // Shared knobs (read directly so the dataset mode needs no `--force`).
    let runs = flag("--runs").and_then(|s| s.parse().ok()).unwrap_or(30usize);
    let k = flag("--k").and_then(|s| s.parse().ok()).unwrap_or(25usize);
    let model_seed = flag("--model-seed").and_then(|s| s.parse().ok()).unwrap_or(1u64);
    let opts = Opts { refutation_runs: runs, dependency_runs: runs, dependency_k: k };

    match flag("--dataset") {
        Some(path) => run_dataset(&path, model_seed, &opts),
        None => run_generate(model_seed, &opts),
    }
}

/// Generate a node's yield (the `find`/`grade` contract) and print each puzzle's
/// metrics, hardest first.
fn run_generate(model_seed: u64, opts: &Opts) {
    let args = FindArgs::from_env();
    let spec = args.spec();
    let label = args.label();

    let t0 = std::time::Instant::now();
    let mut yielded: Vec<(u64, DigitGrid)> = Vec::new();
    let mut attempts = 0usize;
    let mut seed = args.base_seed;
    while (yielded.len() as u64) < args.count && attempts < args.max {
        let mut rng = Rng::from_seed(seed);
        attempts += 1;
        if let AttemptResult::Success(p) = attempt(&mut rng, &spec) {
            yielded.push((seed, p.puzzle.0));
        }
        seed += 1;
    }
    let gen_elapsed = t0.elapsed();

    if yielded.is_empty() {
        eprintln!("pelanek[{label}]: no puzzles in {attempts} attempts");
        std::process::exit(1);
    }

    // Grade the batch.
    let tg = std::time::Instant::now();
    let mut rows: Vec<(u64, f64, f64, String)> = yielded
        .iter()
        .enumerate()
        .map(|(i, (seed, grid))| {
            let m = grade_puzzle(grid, model_seed.wrapping_add(i as u64), opts)
                .expect("generated puzzles are uniquely solvable");
            (*seed, m.refutation_sum, m.dependency, grid.to_line())
        })
        .collect();
    let grade_elapsed = tg.elapsed();

    // Hardest first (highest refutation sum).
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    println!("# refut       depend  seed        puzzle");
    for (seed, refut, depend, line) in &rows {
        println!("{refut:<11.3} {depend:>6.3}  {seed:<10}  {line}");
    }
    eprintln!(
        "pelanek[{label}]: {} puzzles over {attempts} attempts \
         ({:.3}s gen, {:.3}s grade); runs={} k={}",
        rows.len(),
        gen_elapsed.as_secs_f64(),
        grade_elapsed.as_secs_f64(),
        opts.refutation_runs,
        opts.dependency_k,
    );
}

/// Grade a normalized dataset CSV and report metric-vs-human correlations.
fn run_dataset(path: &str, model_seed: u64, opts: &Opts) {
    let limit = flag("--limit").and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("pelanek: cannot read {path:?}: {e}");
            std::process::exit(1);
        }
    };

    // Parse every data row (header `puzzle,label_value,label_raw,weight`).
    let mut all: Vec<(DigitGrid, f64)> = Vec::new();
    let mut skipped = 0usize;
    for line in text.lines().skip(1) {
        let mut cols = line.split(',');
        let (Some(puz), Some(lab)) = (cols.next(), cols.next()) else {
            continue; // blank / malformed line
        };
        match (DigitGrid::parse(puz), lab.trim().parse::<f64>()) {
            (Some(grid), Ok(label)) => all.push((grid, label)),
            _ => skipped += 1,
        }
    }

    // `--limit N` subsamples N puzzles spread EVENLY across the whole file, not the
    // first N rows: the normalized CSVs are grouped by difficulty level, so a prefix
    // would be one constant-label block (a degenerate, useless correlation). An even
    // stride keeps the bounded run spanning every level.
    let samples: Vec<(DigitGrid, f64)> = if limit < all.len() {
        (0..limit).map(|i| all[i * all.len() / limit].clone()).collect()
    } else {
        all
    };

    if samples.is_empty() {
        eprintln!("pelanek: no usable rows in {path:?}");
        std::process::exit(1);
    }

    // Grade every puzzle; drop any that does not solve uniquely.
    let t0 = std::time::Instant::now();
    let mut labels = Vec::new();
    let mut refut = Vec::new();
    let mut depend = Vec::new();
    let mut unsolved = 0usize;
    for (i, (grid, label)) in samples.iter().enumerate() {
        match grade_puzzle(grid, model_seed.wrapping_add(i as u64), opts) {
            Some(m) => {
                labels.push(*label);
                refut.push(m.refutation_sum);
                depend.push(m.dependency);
            }
            None => unsolved += 1,
        }
        if (i + 1) % 50 == 0 {
            eprint!("\r  graded {}/{} ...", i + 1, samples.len());
        }
    }
    eprintln!("\r  graded {} puzzles in {:.1}s          ", labels.len(), t0.elapsed().as_secs_f64());

    let name = std::path::Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or(path);
    println!("dataset {name}: {} graded, {skipped} unparsable, {unsolved} non-unique", labels.len());
    println!(
        "  refutation_sum vs human: pearson={:+.3}  spearman={:+.3}",
        pearson(&refut, &labels),
        spearman(&refut, &labels),
    );
    println!(
        "  dependency     vs human: pearson={:+.3}  spearman={:+.3}   (inverse: bigger = easier)",
        pearson(&depend, &labels),
        spearman(&depend, &labels),
    );
}

/// Read the value following `--name` on the command line, if present.
fn flag(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

/// Pearson product-moment correlation of two equal-length samples. `0.0` for a
/// degenerate (constant) sample, where the coefficient is undefined.
fn pearson(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len() as f64;
    if n == 0.0 {
        return 0.0;
    }
    let mx = x.iter().sum::<f64>() / n;
    let my = y.iter().sum::<f64>() / n;
    let mut sxy = 0.0;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    for (&xi, &yi) in x.iter().zip(y) {
        let (dx, dy) = (xi - mx, yi - my);
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    if sxx == 0.0 || syy == 0.0 {
        return 0.0;
    }
    sxy / (sxx.sqrt() * syy.sqrt())
}

/// Spearman rank correlation: Pearson of the fractional ranks (ties share the
/// average rank), robust to the metrics' nonlinear relationship to human time.
fn spearman(x: &[f64], y: &[f64]) -> f64 {
    pearson(&ranks(x), &ranks(y))
}

/// Fractional ranks of `v` (1-based), assigning tied values their average rank.
fn ranks(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].partial_cmp(&v[b]).unwrap_or(std::cmp::Ordering::Equal));
    let mut r = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i + 1;
        while j < idx.len() && v[idx[j]] == v[idx[i]] {
            j += 1;
        }
        // ranks i..j (0-based) tie; their shared 1-based average is (i+j+1)/2.
        let avg = (i + j + 1) as f64 / 2.0;
        for &k in &idx[i..j] {
            r[k] = avg;
        }
        i = j;
    }
    r
}
