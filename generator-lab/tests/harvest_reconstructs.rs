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
//! The fixtures were harvested through the SIMT warp; we replay through the *scalar* `attempt`,
//! so a green run also re-confirms the two paths agree (they are meant to be lane-for-lane
//! identical). Run with `--release` -- the scalar strip walk is far too slow in debug for the
//! window sweeps.

use std::path::{Path, PathBuf};

use generator_lab::generate::{AttemptResult, attempt};
use generator_lab::harvest::{Record, parse_records};
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/harvest")
}

/// `Some(puzzle line)` if `seed` yields a puzzle for `spec`, else `None`.
fn yielded(seed: u64, spec: &Spec) -> Option<String> {
    match attempt(&mut Rng::from_seed(seed), spec) {
        AttemptResult::Success(p) => Some(p.puzzle.to_line()),
        AttemptResult::NotForced | AttemptResult::NeverFired => None,
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

#[test]
fn harvest_fixtures_reconstruct() {
    let records = load_records();
    for (tag, fx) in &records {
        let spec = fx.spec();

        // A record must reconstruct at least one puzzle, or it only ever checks the negative
        // half (a generator that silently stopped yielding would still pass it).
        assert!(fx.has_positive(), "{tag}: no hit or sample to reconstruct");

        // Exhaustive window: yields <=> recorded, and the puzzle matches.
        if let Some((base, span)) = fx.window {
            for &(seed, _) in &fx.hits {
                assert!(
                    (base..base + span).contains(&seed),
                    "{tag}: hit seed {seed} outside window [{base}, {})",
                    base + span,
                );
            }
            let recorded: std::collections::HashMap<u64, String> = fx.hits.iter().cloned().collect();
            for seed in base..base + span {
                match (yielded(seed, &spec), recorded.get(&seed)) {
                    (Some(got), Some(want)) => {
                        assert_eq!(&got, want, "{tag}: seed {seed} reconstructs a DIFFERENT puzzle")
                    }
                    (Some(got), None) => panic!(
                        "{tag}: seed {seed} now yields a puzzle but the fixture has no hit for it\n  {got}"
                    ),
                    (None, Some(_)) => panic!("{tag}: recorded hit seed {seed} no longer yields"),
                    (None, None) => {}
                }
            }
        }

        // Positive-only samples (and any hits in a windowless record): each must still yield
        // its exact puzzle.
        for (seed, want) in fx.samples.iter().chain(fx.window.is_none().then_some(&fx.hits).into_iter().flatten()) {
            match yielded(*seed, &spec) {
                Some(got) => assert_eq!(&got, want, "{tag}: sample seed {seed} reconstructs a DIFFERENT puzzle"),
                None => panic!("{tag}: sample seed {seed} no longer yields a puzzle"),
            }
        }
    }
}
