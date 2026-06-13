//! Replay every `seed -> puzzle` fixture in `tests/fixtures/harvest/` and assert the
//! generator still maps seeds to puzzles exactly as harvested. This is the regression guard
//! for the strip-generate pipeline: a refactor that is meant to be behaviour-preserving must
//! keep this green; one that changes which puzzle a seed strips to will fail here, loudly and
//! with the offending seed.
//!
//! Two fixture shapes share one format (`generator_lab::harvest`):
//!   - exhaustive `window` records (from `examples/harvest.rs`) pin BOTH directions -- every
//!     recorded `hit` seed reconstructs its puzzle, and every other seed in the window yields
//!     nothing (an exact bijection over the window);
//!   - positive-only `sample` records (the bulk, from the rarity `examples/scan.rs`) pin the
//!     forward direction only -- each `sample` seed still yields its exact puzzle. These are
//!     cheap (one attempt per recorded seed), so the hundreds of scanned specs stay fast.
//!
//! Both solver paths are checked against the same fixtures, in separate tests:
//!   - [`Engine::Scalar`] -- the scalar [`attempt`] (one strip walk per seed);
//!   - [`Engine::Simt`]   -- the W=8 SIMT warp [`GateStream`] (the path the fixtures were
//!     harvested through).
//!
//! The two paths are meant to be lane-for-lane identical, so a green run on both also
//! re-confirms they still agree. This is the fast yield gate that replaces the old
//! full-range `findpar` diff: it only ever drives the known yielders plus each window's
//! (small) negative control, not millions of empty seeds. Run with `--release` -- the strip
//! walk is far too slow in debug for the window sweeps.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use generator_lab::generate::warp_host::{GateStream, Pumped};
use generator_lab::generate::{AttemptResult, attempt};
use generator_lab::harvest::{Record, parse_records};
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/harvest")
}

/// Which solver path produces the yields under test. The fixtures pin both, since the scalar
/// `attempt` and the SIMT warp are meant to be lane-for-lane identical -- a refactor of either
/// that changes a seed's puzzle (or whether it yields at all) fails the matching test.
#[derive(Clone, Copy)]
enum Engine {
    Scalar,
    Simt,
}

impl Engine {
    fn label(self) -> &'static str {
        match self {
            Engine::Scalar => "scalar",
            Engine::Simt => "simt",
        }
    }

    /// The `seed -> puzzle line` map this engine produces over `seeds` for `spec` (only the
    /// yielding seeds appear). The scalar path runs one independent `attempt` per seed; the
    /// SIMT path races the whole seed iterator through one warp and drains it to exhaustion.
    fn run<I: Iterator<Item = u64>>(self, seeds: I, spec: &Spec) -> HashMap<u64, String> {
        match self {
            Engine::Scalar => seeds
                .filter_map(|seed| match attempt(&mut Rng::from_seed(seed), spec) {
                    AttemptResult::Success(p) => Some((seed, p.puzzle.to_line())),
                    AttemptResult::NotForced | AttemptResult::NeverFired => None,
                })
                .collect(),
            Engine::Simt => {
                let mut stream = GateStream::new(seeds, spec);
                let mut found = HashMap::new();
                loop {
                    match stream.pump(4096) {
                        Pumped::Found(seed, p) => {
                            found.insert(seed, p.puzzle.to_line());
                        }
                        Pumped::StepCountReached => {}
                        Pumped::NoMorePuzzles => break,
                    }
                }
                found
            }
        }
    }
}

/// Every record in the dir, tagged with `file: label` for assertion messages. Asserts the
/// directory is non-empty so a wrong path or missing fixtures can't make the suite pass.
fn load_records() -> Vec<(String, Record)> {
    let dir = fixtures_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    paths.sort();

    let mut out = Vec::new();
    for path in &paths {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{file}: {e}"));
        let records = parse_records(&text).unwrap_or_else(|e| panic!("{file}: {e}"));
        for r in records {
            let tag = format!("{file}: {}", r.label());
            out.push((tag, r));
        }
    }
    assert!(!out.is_empty(), "no fixture records in {}", dir.display());
    out
}

/// Drive every fixture through `engine` and assert it reconstructs exactly what was harvested.
fn reconstruct_all(engine: Engine) {
    let eng = engine.label();
    for (tag, fx) in &load_records() {
        let spec = fx.spec();

        // A record must reconstruct at least one puzzle, or it only ever checks the negative
        // half (a generator that silently stopped yielding would still pass it).
        assert!(fx.has_positive(), "{tag}: no hit or sample to reconstruct");

        // Exhaustive window: yields <=> recorded, and the puzzle matches. Race the whole
        // window through the engine once and compare its yield map against the recorded hits
        // seed-by-seed -- a seed present on exactly one side is a regression in either
        // direction (a new yielder, or a lost one).
        if let Some((base, span)) = fx.window {
            for &(seed, _) in &fx.hits {
                assert!(
                    (base..base + span).contains(&seed),
                    "{tag}: hit seed {seed} outside window [{base}, {})",
                    base + span,
                );
            }
            let recorded: HashMap<u64, String> = fx.hits.iter().cloned().collect();
            let found = engine.run(base..base + span, &spec);
            for seed in base..base + span {
                match (found.get(&seed), recorded.get(&seed)) {
                    (Some(got), Some(want)) => {
                        assert_eq!(got, want, "{tag} [{eng}]: seed {seed} reconstructs a DIFFERENT puzzle")
                    }
                    (Some(got), None) => panic!(
                        "{tag} [{eng}]: seed {seed} now yields a puzzle but the fixture has no hit for it\n  {got}"
                    ),
                    (None, Some(_)) => panic!("{tag} [{eng}]: recorded hit seed {seed} no longer yields"),
                    (None, None) => {}
                }
            }
        }

        // Positive-only seeds: every `sample`, plus a windowless record's `hit`s (which carry
        // no negative claim). Each must still yield its exact puzzle. Drive them as one batch
        // (the SIMT warp then fills its lanes from exactly these seeds, not the gaps between).
        let positives: Vec<(u64, &String)> = fx
            .samples
            .iter()
            .chain(fx.window.is_none().then_some(&fx.hits).into_iter().flatten())
            .map(|(seed, puzzle)| (*seed, puzzle))
            .collect();
        if !positives.is_empty() {
            let found = engine.run(positives.iter().map(|&(seed, _)| seed), &spec);
            for (seed, want) in positives {
                match found.get(&seed) {
                    Some(got) => {
                        assert_eq!(got, want, "{tag} [{eng}]: sample seed {seed} reconstructs a DIFFERENT puzzle")
                    }
                    None => panic!("{tag} [{eng}]: sample seed {seed} no longer yields a puzzle"),
                }
            }
        }
    }
}

#[test]
fn harvest_fixtures_reconstruct_scalar() {
    reconstruct_all(Engine::Scalar);
}

#[test]
fn harvest_fixtures_reconstruct_simt() {
    reconstruct_all(Engine::Simt);
}
