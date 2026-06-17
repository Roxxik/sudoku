//! Like the `harvest` example, but harvests an ARBITRARY explicit spec instead of a
//! `--toolbox`-preset one. Where `harvest` only builds train/drill/full unions around a
//! forced kind, this scans whatever per-kind label set you spell out -- the same freedom
//! [`Spec::explicit`] gives -- so you can mine specs the toolbox presets can't express
//! (e.g. force X-Wing while conceding only Naked Pair, or allow an off-branch technique).
//!
//! The spec is layered from `Spec::explicit()` by applying `--allow`/`--force`/`--concede`
//! directives in command order (a later directive on the same kind overrides an earlier one,
//! exactly as the [`Spec`] builder does):
//!   - `--allow NAME`        -- mark the kind `Allowed` (part of the baseline toolbox);
//!   - `--force NAME[:COUNT]` -- mark it `Forced` with required firing COUNT (default 1);
//!   - `--concede NAME`      -- mark it `Conceded` (in scope for the avoid-walk only).
//!
//! Everything else matches `harvest`:
//!   - each hit's `seed puzzle` goes to STDOUT (seed-sorted), for a wrapping script to keep;
//!   - a parseable one-line summary goes to STDERR: `spec=.. base=.. attempts=.. hits=..
//!     next=.. path=simt|scalar`, where `next` (`base + attempts`) is the first un-scanned
//!     seed so the script resumes there.
//!
//! One seed = one attempt; `--attempts N` scans `[seed, seed+N)` exactly -- which seeds yield
//! is a pure function of the seed. Path is chosen per spec exactly as in `harvest`: the W=8
//! SIMT warp where its fused baseline is sound ([`baseline_fast_applicable`] -- both singles,
//! LC both-or-neither, no Forced cheap kind), the scalar [`attempt`] otherwise. The two seed
//! -> puzzle maps agree wherever the warp applies, so the path is a speed choice only.
//!
//! NOTE: a spec with no `--force` has its forcing requirement trivially met, so nearly every
//! minimal puzzle in the toolbox counts as a hit -- harvest a forceless spec only if you mean
//! to. Unlike `harvest`, this writes no fixture file: it only scans and reports.
//!
//! Usage: cargo run --release -p generator-lab --example harvest-custom -- \
//!          [--allow NAME] [--force NAME[:COUNT]] [--concede NAME] ... \
//!          [--seed BASE=1] [--attempts N=100000]

use std::fmt::Write as _;

use generator_lab::cli::parse_force;
use generator_lab::generate::warp_host::{GateStream, Pumped};
use generator_lab::generate::{AttemptResult, attempt, baseline_fast_applicable};
use generator_lab::rng::Rng;
use generator_lab::spec::kinds::NAMES;
use generator_lab::spec::{Spec, Usage};

/// One per-kind label directive, in command order, applied to `Spec::explicit()`.
struct Directive {
    idx: usize,
    usage: Usage,
}

impl Directive {
    /// Canonical token for the summary key, e.g. `allow/x-wing`, `force/x-wing:2`,
    /// `concede/naked-pair`. No spaces, so the whole `spec=` field stays one token.
    fn token(&self) -> String {
        match self.usage {
            Usage::Allowed => format!("allow/{}", NAMES[self.idx]),
            Usage::Forced(1) => format!("force/{}", NAMES[self.idx]),
            Usage::Forced(c) => format!("force/{}:{c}", NAMES[self.idx]),
            Usage::Conceded => format!("concede/{}", NAMES[self.idx]),
        }
    }
}

struct Args {
    directives: Vec<Directive>,
    base: u64,
    attempts: u64,
}

impl Args {
    fn from_env() -> Self {
        let mut directives = Vec::new();
        let mut base = 1u64;
        let mut attempts = 100_000u64;
        let mut it = std::env::args().skip(1);
        // `--allow`/`--concede` ignore any `:COUNT` suffix (only `--force` carries a count),
        // but reuse `parse_force` for its known-kinds error message.
        let mut push = |it: &mut dyn Iterator<Item = String>, usage: fn(u16) -> Usage| {
            match parse_force(&it.next().unwrap_or_default()) {
                Ok((idx, count)) => directives.push(Directive { idx, usage: usage(count) }),
                Err(msg) => {
                    eprintln!("{msg}");
                    std::process::exit(2);
                }
            }
        };
        while let Some(a) = it.next() {
            match a.as_str() {
                "--allow" => push(&mut it, |_| Usage::Allowed),
                "--force" => push(&mut it, Usage::Forced),
                "--concede" => push(&mut it, |_| Usage::Conceded),
                "--seed" => base = it.next().and_then(|s| s.parse().ok()).unwrap_or(base),
                "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
                _ => {}
            }
        }
        if directives.is_empty() {
            eprintln!("need at least one --allow/--force/--concede NAME[:COUNT]");
            std::process::exit(2);
        }
        Args { directives, base, attempts }
    }

    /// Build the explicit spec these directives describe, applied in command order.
    fn spec(&self) -> Spec {
        let mut spec = Spec::explicit();
        for d in &self.directives {
            spec = match d.usage {
                Usage::Allowed => spec.allow(d.idx),
                Usage::Forced(c) => spec.force(d.idx, c),
                Usage::Conceded => spec.concede(d.idx),
            };
        }
        spec
    }

    /// `'+'`-joined directive tokens -- the stable spec key the rarity script uses (no spaces).
    fn key(&self) -> String {
        self.directives.iter().map(Directive::token).collect::<Vec<_>>().join("+")
    }
}

fn main() {
    let args = Args::from_env();
    let spec = args.spec();

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

    // stdout: the seed -> puzzle pairs, for a wrapping script to keep.
    let mut stdout = String::new();
    for (seed, line) in &hits {
        let _ = writeln!(stdout, "{seed} {line}");
    }
    print!("{stdout}");

    // stderr: the parseable resume summary. `next` = first un-scanned seed.
    let next = args.base + args.attempts;
    eprintln!(
        "spec={} base={} attempts={} hits={} next={next} path={}",
        args.key(),
        args.base,
        args.attempts,
        hits.len(),
        if warp { "simt" } else { "scalar" },
    );
}
