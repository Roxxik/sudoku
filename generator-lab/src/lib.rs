//! # generator-lab — clean-slate hidden-quad generator testing grounds
//!
//! A standalone, spec-driven sudoku generator built ONLY to benchmark and tune
//! the performance-critical generation path on native and mobile (wasm). It
//! reproduces core's `random`-method `train`/`drill` generation for the ladder
//! up to HiddenQuad, and nothing else: no local search, no construction, and
//! none of the play-time hint machinery (`Step`/`Deduction`/`focus_cells`).
//! Techniques mutate the board and report only which KIND fired — all the spec
//! gates need.
//!
//! Why separate from core: core's structures pull dual duty (generate hints
//! during play AND generate puzzles), and mixing those two cost profiles blocks
//! tuning the hot path. This crate is the hot path alone, on a clean slate, so
//! we can find what's gainable before touching core. NOT for backport this
//! session — it's a benchable PoC.
//!
//! ## Modules
//! - [`repr`]: the representation layer — the [`Digit`](repr::Digit)/[`Mark`](repr::Mark)
//!   value types, the [`DigitGrid`](repr::DigitGrid)/[`Board`](repr::Board) grids, and
//!   the banded SIMD packings ([`repr::banded`]) every solver/prober/generator is built
//!   on. The whole crate lives on this layer.
//! - [`rng`]: a primitive kept faithful to core (the strip stream reproduces core's
//!   for a given seed). [`fingerprint`]: the shared FNV fingerprint every generator
//!   path folds puzzles with.
//! - [`scan`]: the MRV/Bivalue branch strategies, and (in [`scan::sieve`]) the
//!   popcount-free candidate-count sieve the scan and the fast prober run on.
//! - [`solve`]: the **logic solver** — `LogicSolver` and its fused fast path
//!   `FusedLogicSolver`, the technique-driven, no-backtracking spec gate over the
//!   `repr` layer, parallel to [`probe`].
//! - [`probe`]: the existence/uniqueness probers (`Search`, `Singles`) over the
//!   `repr` layer, the composable [`techniques`](probe::techniques) singles the
//!   `Singles` reference prober drives, plus the native packed W=8 SIMT prober
//!   ([`probe::simt`]).
//! - [`spec`]: compact `train`/`drill` spec, faithful to core's `Spec`, over the
//!   shared technique taxonomy in [`spec::kinds`] (kind indices, `KindMask`,
//!   `DIFFICULTY`/`NAMES`, and the `SolveTrace` a logic solve returns).
//! - [`fill`]: the random full-grid filler (banded-bitboard MRV search), the first
//!   half of every attempt.
//! - [`generate`]: the strip-generate pipeline on the `repr` layer (the [`probe`]
//!   prober + [`solve`] gate). [`generate::random`] is the shipped scalar/wasm
//!   path; [`generate::warp_host`] (native only) is the W=8 SIMT warp host that
//!   batches the strip loop's uniqueness gates onto [`probe::simt`]. SIMT is a
//!   native AVX play; on wasm simd128 the packing ceiling is too small to pay, so
//!   the wasm cdylib ships the scalar path.

#![feature(portable_simd)]
#![feature(const_trait_impl)]
// The SIMT warp host's per-lane resumable strip attempts are compiler-generated
// coroutines (`generate::warp_host::attempt`); the TAIT names the unnameable coroutine
// type so the stream can hold its lanes inline (no boxing, no dyn dispatch).
#![feature(coroutines, coroutine_trait, type_alias_impl_trait)]
#![feature(stmt_expr_attributes)]

pub mod repr;
pub mod cli;
pub(crate) mod counters;
pub mod fill;
pub mod fingerprint;
pub mod generate;
pub mod harvest;
pub mod probe;
pub mod rng;
pub mod scan;
pub mod solve;
pub mod spec;

/// Public seam for the criterion microbench in `benches/warp_pass.rs` (feature =
/// `bench`). The SIMT cheap-closure kernel [`warp_pass_full`](solve::simt) and its
/// `V`/`M` board types are `pub(crate)`, so a separate bench crate cannot call them.
/// Rather than widen the hot path's visibility in normal/wasm builds, this module —
/// compiled ONLY under `--features bench` — re-exports the kernel behind a small
/// [`WarpInput`] fixture so the bench can drive one warp pass on a freshly-loaded
/// board without any production coupling.
#[cfg(feature = "bench")]
pub mod bench_seam {
    use crate::probe::simt::{LANES, M, V, ZERO};
    use crate::repr::DigitGrid;
    use crate::solve::simt::{SolveQuery, load_query, warp_pass_full};

    /// The eight warp lanes loaded with stripped boards, ready for one
    /// [`warp_pass_full`]. `Copy` so the bench can stamp a fresh, identically-loaded
    /// warp for each timed call — the kernel mutates the board toward solved in place,
    /// so without a fresh copy a benchmark loop would measure the no-op steady state.
    #[derive(Clone, Copy)]
    pub struct WarpInput {
        r: [[V; 3]; 9],
        unsolved: [V; 3],
        active: M,
    }

    impl WarpInput {
        /// Number of SIMD lanes in one warp (the max boards per [`WarpInput`]).
        pub const LANES: usize = LANES;

        /// Load up to [`LANES`](Self::LANES) stripped boards into a fresh warp; lanes
        /// past `grids.len()` stay empty/inactive. This is bench *setup* (the
        /// `from_digits` band build is not part of the timed kernel).
        pub fn from_grids(grids: &[DigitGrid]) -> Self {
            let mut r = [[ZERO; 3]; 9];
            let mut unsolved = [ZERO; 3];
            let mut active = [false; LANES];
            for (l, g) in grids.iter().take(LANES).enumerate() {
                load_query(&mut r, &mut unsolved, l, &SolveQuery::from_digits(g));
                active[l] = true;
            }
            WarpInput { r, unsolved, active: M::from_array(active) }
        }

        /// ONE cheap-closure pass across the active lanes — the timed kernel. Returns
        /// per-lane `(changed, dead, solved)`.
        #[inline]
        pub fn pass(&mut self) -> (M, M, M) {
            warp_pass_full(&mut self.r, &mut self.unsolved, self.active)
        }

        /// Drive the warp to fixpoint: repeat [`pass`](Self::pass) until no active lane
        /// makes progress (solved/dead lanes drop out of `active`; a stalled lane that
        /// needs the off-warp subset ladder simply stops the loop). Closer to the real
        /// per-strip baseline cost than a single pass. Returns the pass count.
        #[inline]
        pub fn to_fixpoint(&mut self) -> u32 {
            let mut passes = 0;
            loop {
                let (changed, dead, solved) = self.pass();
                passes += 1;
                self.active &= !(dead | solved);
                if !(changed & self.active).any() {
                    return passes;
                }
            }
        }
    }
}

use spec::Spec;
use spec::kinds::{DIFFICULTY, HIDDEN_QUAD, NUM, Tier, branch_of, tier_of};

/// Build the **Subset-branch** bench spec for a `mode`: 0 = train(HiddenQuad), else
/// drill(HiddenQuad). The HiddenQuad ladder is the lab's standard Subset benchmark
/// target; for any other Expert target use [`expert_spec`].
pub fn subset_spec_for_mode(mode: u32) -> Spec {
    if mode == 0 {
        Spec::train(HIDDEN_QUAD)
    } else {
        Spec::drill(HIDDEN_QUAD)
    }
}

/// Build a train/drill spec for any **Expert** `target` kind index (the target
/// determines its branch — fish or subset — via the taxonomy's `BRANCH`). `drill`
/// selects drill-mode over train-mode. This is the free-target entry the Fish-branch
/// benches/tests drive (e.g. `expert_spec(X_WING, false)`); the Trunk/Intermediate
/// targets are out of scope for the lab (they generate trivially in core).
pub fn expert_spec(target: usize, drill: bool) -> Spec {
    if drill { Spec::drill(target) } else { Spec::train(target) }
}

/// Train-mode spec for a **set** of forced targets — the multi-force generalization of
/// [`Spec::train`]. A kind is Allowed iff it is in SOME target's train scope (Trunk up to
/// that target's tier, plus simpler-or-equal same-branch peers); each target is then
/// Forced (count 1). Combining requirements keeps the per-attempt baseline cost high (no
/// Forced kind can fast-path) while making the puzzle rarer — the "require a W-Wing AND a
/// jellyfish" specs the combo benches sweep. `train_union(&[t])` reduces to
/// `Spec::train(t)` (same masks); callers needing a count > 1 re-`force` after.
pub fn train_union(forces: &[usize]) -> Spec {
    let mut allowed = [false; NUM];
    for &target in forces {
        let target_tier = tier_of(target);
        let target_branch = branch_of(target);
        let target_diff = DIFFICULTY[target];
        for idx in 0..NUM {
            let tt = tier_of(idx);
            if tt > target_tier {
                continue;
            }
            let ok = if tt <= Tier::Intermediate {
                true
            } else {
                branch_of(idx) == target_branch && DIFFICULTY[idx] <= target_diff
            };
            allowed[idx] |= ok;
        }
    }
    let mut spec = Spec::explicit();
    for (idx, &a) in allowed.iter().enumerate() {
        if a {
            spec = spec.allow(idx);
        }
    }
    for &target in forces {
        spec = spec.force(target, 1);
    }
    spec
}

/// Drill-mode spec for a **set** of forced targets — the multi-force generalization of
/// [`Spec::drill`]. The union, with Allowed taking precedence over Conceded: a kind is
/// **Allowed** iff it is below SOME target's tier (the union of the easier tiers);
/// otherwise **Conceded** iff SOME target concedes it at its own tier (the same per-tier
/// rule `Spec::drill` uses: flat-Intermediate concedes every same-tier peer; an
/// Expert/Master target concedes its simpler same-branch peers); each target is then
/// Forced. So the baseline (Allowed | Forced) is the Trunk-up-to-tier plus the targets
/// only — the simpler substitutes are conceded (verify's avoid-walk may use them, the
/// baseline solver may not), which is what makes each forced target fire in the baseline
/// trace rather than being fast-pathed by a simpler peer.
///
/// With both targets in scope, verify's avoid-walk for one target may use the other as a
/// substitute — the correct reading of "requires **both**" (a puzzle solvable with `t2`
/// instead of `t1` does not require `t1`). `drill_union(&[t])` reduces to
/// `Spec::drill(t)` (same masks); callers needing a count > 1 re-`force` after.
pub fn drill_union(forces: &[usize]) -> Spec {
    let mut allowed = [false; NUM];
    let mut conceded = [false; NUM];
    for &t in forces {
        let t_tier = tier_of(t);
        let t_branch = branch_of(t);
        let t_diff = DIFFICULTY[t];
        for idx in 0..NUM {
            if forces.contains(&idx) {
                continue; // forced targets are forced below, never allowed/conceded
            }
            let tt = tier_of(idx);
            if tt < t_tier {
                allowed[idx] = true; // easier tier for this target
            } else if tt == t_tier {
                let concede = match t_tier {
                    Tier::Beginner => false,
                    Tier::Intermediate => true, // flat tier: concede every same-tier peer
                    Tier::Expert | Tier::Master => {
                        branch_of(idx) == t_branch && DIFFICULTY[idx] < t_diff
                    }
                };
                conceded[idx] |= concede;
            }
        }
    }
    let mut spec = Spec::explicit();
    for idx in 0..NUM {
        if allowed[idx] {
            spec = spec.allow(idx); // Allowed precedence over Conceded
        } else if conceded[idx] {
            spec = spec.concede(idx);
        }
    }
    for &t in forces {
        spec = spec.force(t, 1);
    }
    spec
}

#[cfg(test)]
mod union_tests {
    use super::{drill_union, train_union};
    use crate::spec::Spec;
    use crate::spec::kinds::NUM;

    /// The union builders must reduce to the single-target `Spec::train`/`Spec::drill`
    /// for any one kind — same baseline / in-scope / forced masks. This is the contract
    /// the combo benches lean on (a single `--force` must behave exactly like the
    /// single-target spec).
    #[test]
    fn single_target_reduces_to_train_and_drill() {
        for t in 0..NUM {
            let (tu, st) = (train_union(&[t]), Spec::train(t));
            assert_eq!(tu.baseline_mask(), st.baseline_mask(), "train baseline [{t}]");
            assert_eq!(tu.in_scope_mask(), st.in_scope_mask(), "train in_scope [{t}]");
            assert_eq!(tu.forced_mask(), st.forced_mask(), "train forced [{t}]");
            let (du, sd) = (drill_union(&[t]), Spec::drill(t));
            assert_eq!(du.baseline_mask(), sd.baseline_mask(), "drill baseline [{t}]");
            assert_eq!(du.in_scope_mask(), sd.in_scope_mask(), "drill in_scope [{t}]");
            assert_eq!(du.forced_mask(), sd.forced_mask(), "drill forced [{t}]");
        }
    }
}

/// JS-callable bench entry points, exported from the wasm32 cdylib build, timed
/// JS-side with `performance.now()` exactly as a browser measures. Mirrors
/// solver-lab's wasm harness so the mobile front-end carries over.
#[cfg(target_arch = "wasm32")]
mod wasm_exports {
    use crate::rng::Rng;
    use crate::{generate, subset_spec_for_mode};

    /// Run exactly `attempts` strip attempts for `mode` (0=train, 1=drill) from
    /// `seed`, returning a u32-truncated fingerprint over the produced puzzles
    /// (so JS needs no BigInt bridge; 32 bits is ample to catch divergence). JS
    /// times this with `performance.now()`.
    #[unsafe(no_mangle)]
    pub extern "C" fn bench(mode: u32, attempts: u32, seed: u32) -> u32 {
        let spec = subset_spec_for_mode(mode);
        let mut rng = Rng::from_seed(seed as u64);
        let (_stats, fp) = generate::run_attempts(&mut rng, &spec, attempts as usize);
        fp as u32
    }

    /// Number of puzzles produced in `attempts` attempts (the yield), so the JS
    /// harness can report puzzles/sec alongside us/attempt.
    #[unsafe(no_mangle)]
    pub extern "C" fn bench_yield(mode: u32, attempts: u32, seed: u32) -> u32 {
        let spec = subset_spec_for_mode(mode);
        let mut rng = Rng::from_seed(seed as u64);
        let (stats, _fp) = generate::run_attempts(&mut rng, &spec, attempts as usize);
        stats.successes as u32
    }

    /// Grid-sensitive cross-backend determinism fingerprint (u32-truncated):
    /// folds the solution grid + strip order of every attempt, so it diverges if
    /// native and wasm ever disagree on the RNG/fill output (unlike `bench`'s
    /// success-only fp). Must equal the native `determinism_fp` for the seed.
    #[unsafe(no_mangle)]
    pub extern "C" fn det_fp(attempts: u32, seed: u32) -> u32 {
        let mut rng = Rng::from_seed(seed as u64);
        generate::determinism_fp(&mut rng, attempts as usize) as u32
    }
}
