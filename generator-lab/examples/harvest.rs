//! Scan a contiguous seed range for ONE spec and report what it found, so a wrapping script
//! can quantify rarity and resume:
//!   - each hit's `seed puzzle` to STDOUT (seed-sorted), for the script to keep as samples;
//!   - a parseable one-line summary to STDERR: `spec=.. base=.. attempts=.. hits=.. next=..
//!     path=simt|scalar`.
//!
//! `next` is the first un-scanned seed (`base + attempts`), so the script continues there and
//! never rescans the same (often empty) range.
//!
//! One seed = one attempt; `--attempts N` scans `[seed, seed+N)` exactly -- which seeds yield is
//! a pure function of the seed. Most seeds yield nothing, so a rare spec reports `hits=0` over
//! the range and its `1 in X` rarity is then only known to exceed N. The budget is the CLI's
//! job here.
//!
//! Path is chosen per spec (reported as `path=`): the W=8 SIMT warp is the default and races
//! the whole range through one warp. But its vectorized baseline is the fused solver, which
//! records the cheap kinds (singles, locked candidates) fired-or-not rather than by exact count,
//! so it is sound only when [`baseline_fast_applicable`] holds (both singles, LC
//! both-or-neither, no Forced cheap kind) -- the Expert specs. A Beginner or Intermediate spec
//! FORCES a cheap kind (hidden-single / naked-single / locked candidates), so the warp would
//! mis-accept; those fall back to the scalar [`attempt`], which runs the exact `LogicSolver` and
//! yields correctly (the fused solver is fast enough to mine them by the thousand). The two
//! seed -> puzzle maps are identical wherever the warp applies, so the choice is speed only,
//! never which hits are recorded.
//!
//! With `--out PATH` it also writes a single-spec exhaustive-window fixture (the seeds
//! `[seed, seed+N)` were all tried; the hits are the complete yielder set), for the strong
//! bijection check in `tests/harvest_reconstructs.rs`. Without `--out` it only scans and
//! reports -- no file is written. (`window` survives as the FIXTURE keyword -- the range over
//! which the recorded hits are complete -- but the CLI budget is plainly `--attempts`.)
//!
//! Usage: cargo run --release -p generator-lab --example harvest -- \
//!          --force NAME[:COUNT] [--force ...] [--toolbox train|drill|full] \
//!          [--seed BASE=1] [--attempts N=100000] [--out PATH]

use std::fmt::Write as _;
use std::path::PathBuf;

use generator_lab::cli::{Toolbox, build_spec, parse_force, spec_label};
use generator_lab::generate::warp_host::{GateStream, Pumped};
use generator_lab::generate::{AttemptResult, attempt, baseline_fast_applicable};
use generator_lab::harvest::Record;
use generator_lab::rng::Rng;
use generator_lab::spec::kinds::NAMES;

struct Args {
    forces: Vec<(usize, u16)>,
    toolbox: Toolbox,
    base: u64,
    attempts: u64,
    out: Option<PathBuf>,
}

impl Args {
    fn from_env() -> Self {
        let mut forces = Vec::new();
        let mut toolbox = Toolbox::Train;
        let mut base = 1u64;
        let mut attempts = 100_000u64;
        let mut out = None;
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
                "--seed" => base = it.next().and_then(|s| s.parse().ok()).unwrap_or(base),
                "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
                "--out" => out = it.next().map(PathBuf::from),
                _ => {}
            }
        }
        if forces.is_empty() {
            eprintln!("need at least one --force NAME[:COUNT]");
            std::process::exit(2);
        }
        Args { forces, toolbox, base, attempts, out }
    }

    /// `'+'`-joined `NAME[:COUNT]` -- the stable spec key the rarity script uses (no spaces).
    fn key(&self) -> String {
        self.forces
            .iter()
            .map(|&(i, c)| if c == 1 { NAMES[i].to_string() } else { format!("{}:{c}", NAMES[i]) })
            .collect::<Vec<_>>()
            .join("+")
    }
}

fn main() {
    let args = Args::from_env();
    let spec = build_spec(&args.forces, args.toolbox);

    // Pick the path per spec. SIMT warp where it is sound (the default, far faster); scalar
    // `attempt` otherwise. The two seed -> puzzle maps agree wherever the warp applies, so the
    // recorded hits are identical either way -- only the speed differs.
    let warp = baseline_fast_applicable(&spec);
    let hits: Vec<(u64, String)> = if warp {
        // Race the bounded range through the warp; a finite seed iterator drains to NoMorePuzzles.
        let mut stream = GateStream::new(args.base..args.base + args.attempts, &spec);
        let mut hits = Vec::new();
        loop {
            match stream.pump(4096) {
                Pumped::Found(seed, p) => hits.push((seed, p.puzzle.to_line())),
                Pumped::StepCountReached => {}
                Pumped::NoMorePuzzles => break,
            }
        }
        hits.sort_by_key(|&(seed, _)| seed);
        hits
    } else {
        // Scalar fallback (forced-cheap specs the warp can't gate soundly): one independent
        // `attempt` per seed, iterated in seed order, so `hits` is already seed-sorted.
        let mut hits = Vec::new();
        for seed in args.base..args.base + args.attempts {
            if let AttemptResult::Success(p) = attempt(&mut Rng::from_seed(seed), &spec) {
                hits.push((seed, p.puzzle.to_line()));
            }
        }
        hits
    };

    // stdout: the seed -> puzzle pairs, for the script to keep as fixture samples.
    let mut stdout = String::new();
    for (seed, line) in &hits {
        let _ = writeln!(stdout, "{seed} {line}");
    }
    print!("{stdout}");

    // stderr: the parseable resume summary. `next` = first un-scanned seed.
    let next = args.base + args.attempts;
    eprintln!(
        "spec={} toolbox={} base={} attempts={} hits={} next={next} path={}",
        args.key(),
        args.toolbox.label_short(),
        args.base,
        args.attempts,
        hits.len(),
        if warp { "simt" } else { "scalar" },
    );

    // Optional single-spec exhaustive-window fixture (`[base, base+attempts)` all tried).
    if let Some(out) = args.out {
        let mut body = String::new();
        let _ = writeln!(body, "# generator-lab seed -> puzzle harvest fixture (exhaustive window).");
        let _ = writeln!(body, "# Regenerate with: cargo run --release -p generator-lab --example harvest -- \\");
        let _ = writeln!(
            body,
            "#   {}--toolbox {} --seed {} --attempts {} --out <this file>",
            args.forces.iter().map(|&(i, c)| if c == 1 { format!("--force {} ", NAMES[i]) } else { format!("--force {}:{c} ", NAMES[i]) }).collect::<String>(),
            args.toolbox.label_short(),
            args.base,
            args.attempts,
        );
        let _ = writeln!(body, "# Every seed in the window was tried; 'hit' lines are EXACTLY the yielders, so");
        let _ = writeln!(body, "# the test asserts an exact bijection (yields <=> recorded).");
        let record =
            Record { forces: args.forces.clone(), toolbox: args.toolbox, window: Some((args.base, args.attempts)), hits, samples: Vec::new() };
        record.write_block(&mut body);
        if let Some(dir) = out.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            eprintln!("harvest: cannot create {}: {e}", dir.display());
            std::process::exit(1);
        }
        if let Err(e) = std::fs::write(&out, &body) {
            eprintln!("harvest: cannot write {}: {e}", out.display());
            std::process::exit(1);
        }
        eprintln!("harvest[{}]: wrote {}", spec_label(&args.forces), out.display());
    }
}
