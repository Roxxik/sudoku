//! Temporary instrumentation: global counters so we can measure (not guess) how
//! many scans / placements / clones each solver variant performs. The lab is
//! single-threaded, so these are plain mutable statics — no atomics, no locks.
//!
//! Gated behind the off-by-default `count` feature: the `bump_*` calls scattered
//! through the solvers compile to nothing in a normal (timing) build, so they
//! never perturb a benchmark. Enable to read counts:
//!   cargo run --release -p solver-lab --example counts --features count
//! Remove once the variant comparison is settled.

static mut SCANS: u64 = 0;
static mut PLACEMENTS: u64 = 0;
static mut CLONES: u64 = 0;
// Entry clones: one per `has_solution_with` call (the per-query base clone), as
// opposed to internal clones taken at a `solve_first` branch. Splitting the two
// sizes how many clones a "don't clone on the probe's last query" change could
// remove (at most one per probe build).
static mut ENTRY_CLONES: u64 = 0;
static mut BUILDS: u64 = 0;

// SAFETY (all fns): single-threaded use only; each access reads/writes a `u64`
// by value (never takes a reference), so there is no aliasing and no data race.

#[inline(always)]
pub fn bump_scans() {
    #[cfg(feature = "count")]
    unsafe {
        SCANS += 1
    }
}

#[inline(always)]
pub fn bump_placements() {
    #[cfg(feature = "count")]
    unsafe {
        PLACEMENTS += 1
    }
}

/// Add `n` placements at once (the bitboard variant places whole single-waves
/// grouped by digit, so it counts the group size rather than calling
/// [`bump_placements`] per cell).
#[inline(always)]
pub fn bump_placements_by(n: u64) {
    #[cfg(feature = "count")]
    unsafe {
        PLACEMENTS += n
    }
    #[cfg(not(feature = "count"))]
    let _ = n;
}

#[inline(always)]
pub fn bump_clones() {
    #[cfg(feature = "count")]
    unsafe {
        CLONES += 1
    }
}

/// One per `has_solution_with` call (the per-query base clone).
#[inline(always)]
pub fn bump_entry_clone() {
    #[cfg(feature = "count")]
    unsafe {
        ENTRY_CLONES += 1
    }
}

/// One per probe build (`from_board`). Bounds the "skip the last query's clone"
/// opportunity: at most one clone removed per build.
#[inline(always)]
pub fn bump_build() {
    #[cfg(feature = "count")]
    unsafe {
        BUILDS += 1
    }
}

pub fn snapshot() -> (u64, u64, u64) {
    unsafe { (SCANS, PLACEMENTS, CLONES) }
}

/// `(entry_clones, builds)` — the extra split used to size clone-elimination.
pub fn snapshot_clone_split() -> (u64, u64) {
    unsafe { (ENTRY_CLONES, BUILDS) }
}

pub fn reset() {
    unsafe {
        SCANS = 0;
        PLACEMENTS = 0;
        CLONES = 0;
        ENTRY_CLONES = 0;
        BUILDS = 0;
    }
}
