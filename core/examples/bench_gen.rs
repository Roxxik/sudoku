// Benchmark harness for the generator across the full training surface.
//
// Iterates over baseline puzzles, every difficulty tier, every curriculum
// stage in both train (broad) and drill (narrow) modes, every family, and
// the constructor path. The same seed list runs through every row so the
// numbers are directly comparable — pick one slow row and dig in.
//
// Usage: `cargo run --release --example bench_gen -- [--seeds N=3] [--attempts M=2000] [--filter <substr>]`

use std::time::Instant;

use sudoku_core::{
    CURRICULUM, Family, HiddenTripleConstructor, Rng, Spec, Tier, construct_with, make_puzzle,
    make_puzzle_for_spec,
};

struct Args {
    seeds: usize,
    attempts: usize,
    filter: Option<String>,
}

fn parse_args() -> Args {
    let mut out = Args {
        seeds: 3,
        attempts: 2000,
        filter: None,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--seeds" => out.seeds = next_usize(&mut iter, "--seeds"),
            "--attempts" => out.attempts = next_usize(&mut iter, "--attempts"),
            "--filter" => out.filter = Some(iter.next().unwrap_or_else(|| usage())),
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => usage(),
        }
    }
    out
}

fn next_usize(iter: &mut impl Iterator<Item = String>, name: &str) -> usize {
    iter.next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            eprintln!("{} requires a positive integer", name);
            std::process::exit(2);
        })
}

fn usage() -> ! {
    eprintln!(
        "usage: bench_gen [--seeds N=3] [--attempts M=2000] [--filter <substr>]",
    );
    std::process::exit(2);
}

fn print_help() {
    println!("bench_gen — sweep the generator across the training surface.");
    println!();
    println!("  --seeds N       seeds per spec (default 3)");
    println!("  --attempts M    max_attempts per generate (default 2000)");
    println!("  --filter S      only run rows whose label contains S");
    println!();
    println!("Output: per-row success rate, total time, total attempts,");
    println!("slowest individual run. Trailing summary ranks the slowest rows.");
}

const FAMILIES: &[Family] = &[
    Family::Singles,
    Family::LockedCandidates,
    Family::NakedSubset,
    Family::HiddenSubset,
    Family::Fish,
    Family::TurbotFish,
    Family::Wing,
    Family::FinnedFish,
];

fn family_label(f: Family) -> &'static str {
    match f {
        Family::Singles => "singles",
        Family::LockedCandidates => "locked-candidates",
        Family::NakedSubset => "naked-subset",
        Family::HiddenSubset => "hidden-subset",
        Family::Fish => "fish",
        Family::TurbotFish => "turbot-fish",
        Family::Wing => "wing",
        Family::FinnedFish => "finned-fish",
    }
}

#[derive(Clone)]
struct Row {
    label: String,
    seeds_run: usize,
    successes: usize,
    total_time: f64,
    total_attempts: usize,
    hit_limit: usize,
    max_time: f64,
    max_seed: u64,
}

const SLOW_THRESHOLD_SEC: f64 = 1.0;

fn print_header() {
    println!(
        "  {:<34} {:>5}  {:>8}  {:>8}  {:>10}  {:>20}",
        "label", "ok", "total", "atts", "per-att", "slowest"
    );
    println!(
        "  {:-<34} {:->5}  {:->8}  {:->8}  {:->10}  {:->20}",
        "", "", "", "", "", ""
    );
}

fn print_row(r: &Row) {
    let marker = if r.max_time >= SLOW_THRESHOLD_SEC { "*" } else { " " };
    let per_att_ms = if r.total_attempts > 0 {
        r.total_time * 1000.0 / r.total_attempts as f64
    } else {
        0.0
    };
    let limit_tag = if r.hit_limit > 0 { "+" } else { " " };
    println!(
        "{} {:<34} {:>2}/{:<2}  {:>6.2}s  {:>6}{}  {:>7.2} ms  {:>9.2}s (#{}) ",
        marker,
        r.label,
        r.successes,
        r.seeds_run,
        r.total_time,
        r.total_attempts,
        limit_tag,
        per_att_ms,
        r.max_time,
        r.max_seed,
    );
}

fn bench<F>(label: &str, seeds: &[u64], filter: Option<&str>, rows: &mut Vec<Row>, mut f: F)
where
    F: FnMut(u64) -> (bool, usize),
{
    if let Some(s) = filter {
        if !label.contains(s) {
            return;
        }
    }
    let mut r = Row {
        label: label.to_string(),
        seeds_run: seeds.len(),
        successes: 0,
        total_time: 0.0,
        total_attempts: 0,
        hit_limit: 0,
        max_time: 0.0,
        max_seed: seeds.first().copied().unwrap_or(0),
    };
    for &seed in seeds {
        let t0 = Instant::now();
        let (success, attempts) = f(seed);
        let dt = t0.elapsed().as_secs_f64();
        r.total_time += dt;
        if dt > r.max_time {
            r.max_time = dt;
            r.max_seed = seed;
        }
        if success {
            r.successes += 1;
        } else {
            r.hit_limit += 1;
        }
        r.total_attempts += attempts;
    }
    print_row(&r);
    rows.push(r);
}

fn bench_spec(
    label: &str,
    spec: &Spec,
    seeds: &[u64],
    filter: Option<&str>,
    rows: &mut Vec<Row>,
    max_attempts: usize,
) {
    bench(label, seeds, filter, rows, |seed| {
        let mut rng = Rng::from_seed(seed);
        match make_puzzle_for_spec(&mut rng, spec, max_attempts) {
            Some(fr) => (true, fr.attempts),
            None => (false, max_attempts),
        }
    });
}

fn main() {
    let args = parse_args();
    let seeds: Vec<u64> = (1..=(args.seeds as u64)).collect();
    println!("bench_gen — seeds {:?}, max_attempts {}", seeds, args.attempts);
    if let Some(f) = &args.filter {
        println!("filter: {:?}", f);
    }
    println!();

    let mut rows: Vec<Row> = Vec::new();
    let filter = args.filter.as_deref();

    println!("== Baseline ==");
    print_header();
    bench(
        "baseline make_puzzle(false)",
        &seeds,
        filter,
        &mut rows,
        |seed| {
            let mut rng = Rng::from_seed(seed);
            let _ = make_puzzle(&mut rng, false);
            (true, 1)
        },
    );
    bench(
        "baseline make_puzzle(true)",
        &seeds,
        filter,
        &mut rows,
        |seed| {
            let mut rng = Rng::from_seed(seed);
            let _ = make_puzzle(&mut rng, true);
            (true, 1)
        },
    );
    println!();

    println!("== Tiers ==");
    print_header();
    for &t in Tier::ALL {
        let spec = Spec::tier(t);
        bench_spec(
            &format!("tier {}", t.key()),
            &spec,
            &seeds,
            filter,
            &mut rows,
            args.attempts,
        );
    }
    println!();

    println!("== Curriculum (train, broad spec) ==");
    print_header();
    for stage in CURRICULUM {
        let spec = stage.broad_spec();
        bench_spec(
            &format!("train {}", stage.focus.cli_name()),
            &spec,
            &seeds,
            filter,
            &mut rows,
            args.attempts,
        );
    }
    println!();

    println!("== Curriculum (drill, narrow spec) ==");
    print_header();
    for stage in CURRICULUM {
        let spec = stage.drill_spec();
        bench_spec(
            &format!("drill {}", stage.focus.cli_name()),
            &spec,
            &seeds,
            filter,
            &mut rows,
            args.attempts,
        );
    }
    println!();

    println!("== Families (allow_all, require_family) ==");
    print_header();
    for &f in FAMILIES {
        let spec = Spec::allow_all().require_family(f, 1);
        bench_spec(
            &format!("family {}", family_label(f)),
            &spec,
            &seeds,
            filter,
            &mut rows,
            args.attempts,
        );
    }
    println!();

    println!("== Constructors ==");
    print_header();
    // HiddenTripleConstructor caps at 1000 attempts internally per call to
    // construct_with, separate from the spec generator's attempt counter.
    let constructor_spec = Spec::allow_up_to(sudoku_core::TechniqueKind::HiddenSingle)
        .require(sudoku_core::TechniqueKind::HiddenTriple, 1);
    let constructor = HiddenTripleConstructor::for_spec(constructor_spec);
    bench(
        "construct hidden-triple",
        &seeds,
        filter,
        &mut rows,
        |seed| {
            let mut rng = Rng::from_seed(seed);
            match construct_with(&constructor, &mut rng, 1000) {
                Some((_, attempts)) => (true, attempts),
                None => (false, 1000),
            }
        },
    );
    println!();

    if rows.is_empty() {
        println!("(no rows ran — check --filter)");
        return;
    }

    let mut slowest = rows.clone();
    slowest.sort_by(|a, b| b.total_time.partial_cmp(&a.total_time).unwrap());
    let top = slowest.len().min(10);
    println!("== Slowest {} ==", top);
    print_header();
    for r in slowest.iter().take(top) {
        print_row(r);
    }
    println!();

    let grand_time: f64 = rows.iter().map(|r| r.total_time).sum();
    let grand_attempts: usize = rows.iter().map(|r| r.total_attempts).sum();
    let total_failures: usize = rows.iter().map(|r| r.hit_limit).sum();
    let total_successes: usize = rows.iter().map(|r| r.successes).sum();
    let total_runs: usize = rows.iter().map(|r| r.seeds_run).sum();
    println!(
        "Grand total: {:.2}s across {} rows ({} runs), {} attempts, {}/{} ok, {} hit the attempt limit",
        grand_time,
        rows.len(),
        total_runs,
        grand_attempts,
        total_successes,
        total_runs,
        total_failures,
    );
}
