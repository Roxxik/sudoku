//! scalar == SIMT **prober** verdict equivalence — a property DISTINCT from SIMT == core
//! (`logic_equiv.rs`). The packed prober's uniqueness verdict must match the scalar
//! `Search<Bivalue>` on every gate a real strip walk hits.
//!
//! Why this is its own test, not covered by the seed->puzzle map: a prober that wrongly
//! reports a non-unique board as *unique* still produces the correct puzzle (the baseline
//! gate fails to solve the non-unique board and the strip reverts anyway) — so the output
//! is identical while the work is vastly larger. That "false unique" is invisible to any
//! end-to-end test and only a direct verdict comparison catches it. The corpus is harvested
//! from a cheap spec, but the prover logic it exercises is the same the expensive specs
//! use, so agreement here is spec-rarity-independent.

use generator_lab::generate::warp_host::collect_probes;
use generator_lab::solve::simt::resolve_probes;
use generator_lab::subset_spec_for_mode;

/// Harvest a probe corpus (each paired with the scalar verdict) and assert the SIMT prober
/// reproduces every verdict.
fn check(label: &str, mode: u32, base_seed: u64, attempts: usize) {
    let spec = subset_spec_for_mode(mode);
    let corpus = collect_probes(base_seed, &spec, attempts);
    assert!(
        corpus.len() > 500,
        "{label}: corpus too small to be meaningful ({})",
        corpus.len()
    );

    // Teeth: the corpus must exercise BOTH verdicts, else comparing an all-one-verdict
    // corpus couldn't catch a one-directional bug (e.g. the silent "false unique").
    let nonunique = corpus.iter().filter(|(_, b)| *b).count();
    assert!(
        nonunique > 0 && nonunique < corpus.len(),
        "{label}: corpus must contain both unique and non-unique gates (nonunique {nonunique}/{})",
        corpus.len()
    );

    let probes: Vec<_> = corpus.iter().map(|(p, _)| *p).collect();
    let simt = resolve_probes(&probes);
    assert_eq!(simt.len(), corpus.len());

    let mut diverged = 0usize;
    let mut first: Option<(usize, bool, bool)> = None;
    for (i, ((_, scalar), &got)) in corpus.iter().zip(&simt).enumerate() {
        if *scalar != got {
            diverged += 1;
            first.get_or_insert((i, *scalar, got));
        }
    }
    assert_eq!(
        diverged, 0,
        "{label}: {diverged}/{} prober verdicts diverged from scalar (first: probe {:?} scalar/simt)",
        corpus.len(),
        first
    );
}

#[test]
fn prober_matches_scalar_train() {
    check("train", 0, 1, 300);
}

#[test]
fn prober_matches_scalar_drill() {
    check("drill", 1, 1, 300);
}

/// A different seed base, to vary the boards the gates land on.
#[test]
fn prober_matches_scalar_offset() {
    check("train-offset", 0, 5000, 200);
    check("drill-offset", 1, 5000, 200);
}
