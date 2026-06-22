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
//! With `--calibrate` it also prints a paste-ready `THRESHOLDS` table: per technique, the
//! 33rd/66th `hardness_score` percentile over its pooled train+drill puzzles (the even-thirds
//! sub-tier cut points).
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
use generator_lab::grade::{Signals, band_calibrated, grade_one, hardness_score};
use generator_lab::repr::DigitGrid;
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{self, NAMES};

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

/// Format one spec's band distribution + raw signal ranges.
fn report(node: Node, sigs: &[Signals]) -> String {
    let spec = node.spec();
    let label = node.label();
    if sigs.is_empty() {
        return format!("{label:<28} no puzzles");
    }
    let count = sigs.len();
    let mut bands = [0usize; 3];
    for s in sigs {
        bands[band_calibrated(&spec, s).min(2)] += 1;
    }
    let max_by = |f: fn(&Signals) -> u32| sigs.iter().map(f).max().unwrap();
    let avg_by = |f: fn(&Signals) -> u32| sigs.iter().map(|s| f(s) as f64).sum::<f64>() / count as f64;
    let pct = |c: usize| 100.0 * c as f64 / count as f64;
    let scarce_vals: Vec<f64> =
        sigs.iter().filter(|s| s.scarcity != u32::MAX).map(|s| s.scarcity as f64).collect();
    let scarce_avg = if scarce_vals.is_empty() {
        f64::NAN
    } else {
        scarce_vals.iter().sum::<f64>() / scarce_vals.len() as f64
    };
    let score_max = sigs.iter().map(|s| hardness_score(s)).fold(0.0_f64, f64::max);
    let score_avg = sigs.iter().map(|s| hardness_score(s)).sum::<f64>() / count as f64;
    format!(
        "{label:<28} n={count:<4} gentle={:>3.0}% medium={:>3.0}% spicy={:>3.0}% | \
         bott(avg {:.2} max {}) dryRun(avg {:.2} max {}) dryF(avg {:.2} max {}) \
         scarce(avg {:.1}) scan(avg {:.1} max {}) score(avg {:.2} max {:.2})",
        pct(bands[0]), pct(bands[1]), pct(bands[2]),
        avg_by(|s| s.bottleneck_count), max_by(|s| s.bottleneck_count),
        avg_by(|s| s.longest_dry_run), max_by(|s| s.longest_dry_run),
        avg_by(|s| s.dry_firings), max_by(|s| s.dry_firings),
        scarce_avg,
        avg_by(|s| s.scan_work), max_by(|s| s.scan_work),
        score_avg, score_max,
    )
}

/// Print the paste-ready `THRESHOLDS` rows: per target, the 33rd/66th `hardness_score`
/// percentile over its pooled train+drill signals, plus the band split those cuts produce.
fn calibrate(targets: &[usize], nodes: &[Node], results: &[Mutex<Vec<Signals>>]) {
    println!("=== CALIBRATION (paste into grade::THRESHOLDS; pooled train+drill, even thirds) ===");
    for &t in targets {
        let mut scores: Vec<f64> = nodes
            .iter()
            .enumerate()
            .filter(|(_, nd)| nd.target == t)
            .flat_map(|(i, _)| results[i].lock().unwrap().iter().map(hardness_score).collect::<Vec<_>>())
            .collect();
        if scores.is_empty() {
            continue;
        }
        scores.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let q = |p: f64| scores[((p * scores.len() as f64) as usize).min(scores.len() - 1)];
        let (lo, hi) = (q(1.0 / 3.0), q(2.0 / 3.0));
        let g = scores.iter().filter(|&&s| s < lo).count();
        let m = scores.iter().filter(|&&s| s >= lo && s < hi).count();
        let sp = scores.iter().filter(|&&s| s >= hi).count();
        println!("    ({lo:.4}, {hi:.4}), // {:<14} -> g{g} m{m} s{sp} (n={})", NAMES[t], scores.len());
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

    // Phase 2: per-spec band distribution (train block, then drill block).
    println!("=== TRAIN (isolated) ===");
    for (i, nd) in nodes.iter().enumerate() {
        if !nd.drill {
            println!("{}", report(*nd, &results[i].lock().unwrap()));
        }
    }
    println!("=== DRILL (isolated) ===");
    for (i, nd) in nodes.iter().enumerate() {
        if nd.drill {
            println!("{}", report(*nd, &results[i].lock().unwrap()));
        }
    }

    if arg_flag("--calibrate") {
        calibrate(targets, &nodes, &results);
    }
}
