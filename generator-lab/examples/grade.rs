//! Generate a campaign node's yield for a `--force`/`--toolbox` spec, run the per-node
//! relative difficulty grader over the batch, and print each puzzle's signals, combined
//! score, and band — the "eyeball a node's banded output" tool the grader plan's open
//! decisions (weights, band count) are meant to be tuned against.
//!
//! Same generation contract as `examples/find`: one seed = one attempt, seeds consumed
//! from `--seed` upward until `--count` puzzles surface (or `--max` attempts), each walk a
//! pure function of its seed. The grader then bands the whole batch *relative to itself*
//! (it produces no portable score — see `docs/campaign-grader-plan.md`).
//!
//! Usage: cargo run --release -p generator-lab --example grade -- \
//!          --force NAME[:COUNT] [--force ...] [--toolbox train|drill|full] \
//!          [--seed BASE=1] [--count N=1] [--max M=unlimited] [--bands B=3]

use generator_lab::cli::FindArgs;
use generator_lab::generate::{AttemptResult, attempt};
use generator_lab::grade::{DEFAULT_BANDS, Weights, band_name3, grade_node};
use generator_lab::repr::DigitGrid;
use generator_lab::rng::Rng;
use generator_lab::spec::kinds::NAMES;

fn main() {
    let args = FindArgs::from_env();
    let spec = args.spec();
    let label = args.label();
    let toolbox = args.toolbox.label();
    // `--bands` is grader-specific, so it is read here rather than baked into `FindArgs`.
    let n_bands = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--bands")
        .and_then(|w| w[1].parse::<usize>().ok())
        .filter(|&b| b >= 1)
        .unwrap_or(DEFAULT_BANDS);

    // Collect the node's yield: (seed, puzzle grid).
    let t0 = std::time::Instant::now();
    let mut yielded: Vec<(u64, DigitGrid)> = Vec::new();
    let mut attempts = 0u64;
    let mut seed = args.base_seed;
    while (yielded.len() as u64) < args.count && (attempts as usize) < args.max {
        let mut rng = Rng::from_seed(seed);
        attempts += 1;
        if let AttemptResult::Success(p) = attempt(&mut rng, &spec) {
            yielded.push((seed, p.puzzle.0));
        }
        seed += 1;
    }
    let gen_elapsed = t0.elapsed();

    if yielded.is_empty() {
        eprintln!("grade[{label}] toolbox={toolbox}: no puzzles in {attempts} attempts");
        std::process::exit(1);
    }

    // Grade + band the batch (relative to itself).
    let grids: Vec<DigitGrid> = yielded.iter().map(|(_, g)| g.clone()).collect();
    let tg = std::time::Instant::now();
    let graded = grade_node(&spec, &grids, &Weights::default(), n_bands);
    let grade_elapsed = tg.elapsed();

    // Order spiciest-first within the print, so the hardest puzzles lead each run.
    let mut order: Vec<usize> = (0..graded.len()).collect();
    order.sort_by(|&a, &b| graded[b].score.partial_cmp(&graded[a].score).unwrap());

    let band_tag = |band: usize| -> String {
        if n_bands == 3 { band_name3(band).to_string() } else { format!("b{band}/{n_bands}") }
    };

    // stdout: the table (clean, greppable).
    println!(
        "# band score  bott dryF dryRun scarce scan  seed       puzzle"
    );
    let mut band_counts = vec![0usize; n_bands];
    for &i in &order {
        let g = &graded[i];
        let (seed, grid) = &yielded[i];
        band_counts[g.band] += 1;
        let s = &g.signals;
        let scarce = if s.scarcity == u32::MAX { "-".to_string() } else { s.scarcity.to_string() };
        println!(
            "{:<6} {:.3} {:>4} {:>4} {:>6} {:>6} {:>4}  {:<10} {}",
            band_tag(g.band),
            g.score,
            s.bottleneck_count,
            s.dry_firings,
            s.longest_dry_run,
            scarce,
            s.scan_work,
            seed,
            grid.to_line(),
        );
    }

    // stderr: the summary.
    let forced = forced_names(&args);
    let band_summary: Vec<String> = (0..n_bands)
        .map(|b| format!("{}={}", band_tag(b), band_counts[b]))
        .collect();
    eprintln!(
        "grade[{label}] toolbox={toolbox} forced=[{forced}]: {} puzzles over {attempts} attempts \
         ({:.3}s gen, {:.3}ms grade); bands {}",
        graded.len(),
        gen_elapsed.as_secs_f64(),
        grade_elapsed.as_secs_f64() * 1e3,
        band_summary.join(" "),
    );
}

/// The human-readable forced-kind list for the run summary.
fn forced_names(args: &FindArgs) -> String {
    args.forces
        .iter()
        .map(|&(idx, c)| if c == 1 { NAMES[idx].to_string() } else { format!("{}:{c}", NAMES[idx]) })
        .collect::<Vec<_>>()
        .join(", ")
}
