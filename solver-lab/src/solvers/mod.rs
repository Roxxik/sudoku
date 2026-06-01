//! Fast-solver variants under comparison.
//!
//! Every variant implements [`UniqProber`], the exact interface the generator's
//! strip loop uses: build a reusable existence probe from a (stripped) board,
//! then ask, per alternate digit, whether placing it yields a completion. This
//! is the hot path that dominates generation (~49% of total time in profiling),
//! so it is what we benchmark and what we will port the winner of back into
//! `core::uniqueness`.
//!
//! ## Adding a variant
//!
//! 1. Create a module here implementing [`UniqProber`].
//! 2. Add `pub mod <name>;` below.
//! 3. Add one line — `<name>::Probe,` — to the [`register!`] list.
//!
//! That single list is the whole registry: it drives the native bench, the
//! correctness tests, and the wasm exports (which the JS front-ends discover at
//! runtime), so nothing else — no bench array, no test fn, no JS edit, no wasm
//! export — needs touching. Run `solver-lab/check.sh` to verify + build the web
//! wasm in one step.

use crate::grid::Board;
use crate::rng::Rng;

pub mod banded;
pub mod batch;
pub mod bitboard;
pub mod bitboard_fb;
pub mod bitboard_simd;
pub mod light;
pub mod lightbv;
pub mod propagate;
pub mod simd_dense;

/// A reusable existence probe built once from a board, then queried per
/// (cell, digit) placement. Mirrors `core::uniqueness::ExistenceProbe`.
pub trait UniqProber {
    /// Human-readable name for bench/test output.
    const NAME: &'static str;

    /// Build the probe from a board carrying *naked* candidate masks (the
    /// strip-loop invariant: placements only, no technique eliminations).
    fn from_board(board: &Board) -> Self;

    /// True iff placing digit `d` (1..=9) at the empty cell `i` of the board
    /// this probe was built from yields a grid with at least one completion.
    ///
    /// Takes `&mut self` so a prober may carry mutable scratch reused across
    /// queries instead of cloning per call.
    fn has_solution_with(&mut self, i: usize, d: u8) -> bool;

    /// True iff *some* alternate digit in `alts` (a candidate bitmask) placed at
    /// cell `i` yields a completion — i.e. the strip made `i` non-unique. This is
    /// the exact question the strip loop asks: it only needs whether *any*
    /// alternate solves, and stops at the first that does.
    ///
    /// The default loops [`has_solution_with`], reproducing the old call site
    /// exactly. A prober may override to exploit that it is dropped right after
    /// this returns: the *last* alternate it tries need not preserve the base, so
    /// it can be solved in place rather than on a clone. That removes at most one
    /// clone per probe build and adds nothing to the per-placement path, so an
    /// override can only ever match or beat the default.
    fn any_alt_solves(&mut self, i: usize, alts: crate::grid::Mask) -> bool {
        for d in crate::grid::iter_digits(alts) {
            if self.has_solution_with(i, d) {
                return true;
            }
        }
        false
    }
}

/// One registered prober: its name plus type-erased entry points for the three
/// consumers (bench timing, correctness check, wasm). Built only by
/// [`register!`]; see [`REGISTRY`].
pub struct Variant {
    /// [`UniqProber::NAME`].
    pub name: &'static str,
    /// Run `attempts` strip attempts from `seed`; returns `(fingerprint,
    /// total_givens)`. This is `harness::fingerprint_run::<P>`.
    pub fingerprint: fn(usize, u64) -> (u64, usize),
    /// Assert this prober agrees with the oracle (panics on mismatch). This is
    /// `check::check_prober::<P>`.
    pub check: fn(),
    /// One tracked strip attempt (the `find_quad` "real deal" workload): returns
    /// the most-stripped accepted state whose baseline trace uses a hidden quad,
    /// if any. This is `generate::quad_attempt::<P>`.
    pub quad_attempt: fn(&mut Rng) -> Option<Board>,
}

/// Expand a list of prober types into [`REGISTRY`]. The list is the single
/// source of truth for the whole lab — see the module docs.
macro_rules! register {
    ($($ty:ty),+ $(,)?) => {
        /// Every registered prober, in display order. `REGISTRY[0]` is the
        /// baseline that bench speedups are relative to.
        pub const REGISTRY: &[Variant] = &[
            $(Variant {
                name: <$ty as UniqProber>::NAME,
                fingerprint: crate::harness::fingerprint_run::<$ty>,
                check: crate::check::check_prober::<$ty>,
                quad_attempt: crate::generate::quad_attempt::<$ty>,
            }),+
        ];
    };
}

register!(
    simd_dense::Probe, // baseline — keep first
    propagate::Probe,
    batch::Probe,
    light::Probe,
    lightbv::Probe,
    bitboard::Probe,
    bitboard_simd::Probe,
    bitboard_fb::Probe,
    banded::Probe,
);
