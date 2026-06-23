//! `grade` — the **per-node, relative** campaign difficulty grader (see
//! `docs/campaign-grader-plan.md`). It takes the puzzles one campaign node yields, sorts
//! them against *each other* on how the node's forced technique is distributed across the
//! easiest-first solve, and cuts them into difficulty bands.
//!
//! It is deliberately NOT a portable difficulty score and NOT the future
//! `GENERATION-RULES.md` grader: the player-facing tier is fixed across a node (the spec
//! pins the forced technique, baseline toolbox, and ceiling), so every signal here is
//! *sub-tier* — how many times the forced technique must fire, how brutal the dry grinds
//! between firings are, how scarce the deductions are at each stall, and the overall scan
//! work. All four come off a single easiest-first path (the existing non-branching solver,
//! instrumented as [`solve_graded`](crate::solve::solve_graded)); there is no
//! quantification over solve paths and no branching solver.
//!
//! Pipeline: [`solve_graded`] produces a [`GradeTrace`] per puzzle → [`signals_of`] reads
//! the four signals off it → [`grade_batch`] rank-normalizes each signal across the node's
//! batch, weighted-sums them (dry runs highest, then count, scarcity, scan work), and
//! quantile-cuts the order into bands. [`grade_node`] wires a spec + a batch of puzzle
//! grids straight through.

use std::cmp::Ordering;

use crate::repr::banded::{Bands, RowMajor};
use crate::repr::{DigitGrid, Marks, SolverState};
use crate::solve::{GradeTrace, TrunkProfile, solve_graded, trunk_profile, trunk_profiles_rand};
use crate::spec::Spec;
use crate::spec::kinds::{Branch, DIFFICULTY, KindMask, NAKED_PAIR, NUM, SWORDFISH, XYZ_WING, branch_of};

/// The default band count — gentle / medium / spicy, the plan's natural per-node cut.
pub const DEFAULT_BANDS: usize = 3;

/// The four sub-tier difficulty signals read off one easiest-first solve. All are oriented
/// in the *raw* direction the trace produces them; [`grade_batch`] applies the
/// harder-direction orientation when it ranks (only [`scarcity`](Self::scarcity) is "lower
/// is harder").
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Signals {
    /// **Signal 1** — how many times the node's forced technique `T` must fire on the
    /// easiest-first path. One forced firing then singles-to-the-end is far gentler than
    /// three. Higher is harder.
    pub bottleneck_count: u32,
    /// **Signal 2a** — how many forced firings were *dry* (the cheap closure right after
    /// placed no cell, so the hard scan bought no reward). Higher is harder.
    pub dry_firings: u32,
    /// **Signal 2b** — the longest run of consecutive harder steps that each placed no
    /// cell: the back-to-back hard-scan grind the brainstorm flagged as the nastiest.
    /// This is the **primary** signal. Higher is harder.
    pub longest_dry_run: u32,
    /// **Signal 3** — scarcity at the tightest bottleneck, as the `min` over the forced
    /// firings of the cheap proxy (the eliminations that firing made). Fewer eliminations
    /// = a tighter, harder-to-spot stall, so **lower is harder** here. [`u32::MAX`] when
    /// the puzzle had no bottleneck (it then ranks as the least scarce / easiest).
    pub scarcity: u32,
    /// **Signal 4** — total harder steps (`counts[k]` for `k >= NAKED_PAIR`), a mild
    /// scan-work term. Lowest weight; breaks ties between puzzles matching on 1-3. Higher
    /// is harder.
    pub scan_work: u32,
    /// **S5 (open)** — live-candidate population at the *tightest* bottleneck (the
    /// fewest-elim firing, the one [`scarcity`](Self::scarcity) is taken over). The primary
    /// *continuous* signal of `docs/grader-granular-scoring.md`: ~40..200, so it discriminates
    /// where the integer signals tie. `0` when the puzzle had no bottleneck. Orientation is
    /// per-branch and calibrated (see [`granular_score`]).
    pub open: u32,
    /// **S6 (depth)** — cells already placed when the *first* bottleneck fires. `0` with no
    /// bottleneck. Continuous (0..81); a late first stall means the cheap closure carried the
    /// solver a long way before the technique was needed.
    pub depth: u32,
    /// **S8 (cascade)** — cells the cheap closure placed immediately after the *tightest*
    /// bottleneck firing (the magnitude of the dry/payoff flag). `0` with no bottleneck.
    /// Higher = a longer unlock = gentler, so it is oriented "lower is harder" at score time.
    pub cascade: u32,
    /// **S3' (elim sum)** — total eliminations summed over *all* bottleneck firings. With the
    /// [`bottleneck_count`](Self::bottleneck_count) it gives the mean, a finer refinement of
    /// `min`-only [`scarcity`](Self::scarcity) for the multi-firing (subset) nodes. `0` with no
    /// bottleneck. (For the single-forced UI specs `mean == min == scarcity`.)
    pub elim_sum: u32,
    /// **S7 (alts)** — productive deduction instances across the allowed harder toolbox at the
    /// *tightest* stall (Tier B: the honest scarcity, the `live`-style count). Fewer = a tighter
    /// needle hunt = harder; stays live where `elims`/`scarcity` is structurally pinned (wings).
    /// `0` with no bottleneck (no stall to count at).
    pub alts: u32,

    // --- Tier D: trajectory features (`docs/grader-continuous-scoring.md` §4) -------------
    // Aggregates over the WHOLE bottleneck-firing sequence, not just the tightest/first stall.
    // For a single-firing puzzle each reduces to (a monotone reparam of) its tightest-stall
    // counterpart, so Tier A/B/C behaviour is recoverable; they only add resolution on the
    // multi-firing nodes (subset drills, the wings) the single-stall reduction is lossiest on.
    /// **D1 (tight integral)** — `Σ 1/(1 + alts_i)` over the bottleneck firings: total
    /// stuck-ness across the solve, not just the worst stall. Distinguishes one-hard-stall from
    /// many-medium-stalls puzzles that share a tightest [`alts`](Self::alts). `0` with no
    /// bottleneck. Higher = harder (more, tighter stalls).
    pub tight_integral: f64,
    /// **D2 (open sum)** — `Σ open_i` over the bottleneck firings: the total candidate
    /// population the solver scanned over the solve. Its mean (`open_sum / bottleneck_count`,
    /// taken in [`raw_granular`]) generalises the single-stall [`open`](Self::open). `0` with no
    /// bottleneck.
    pub open_sum: u32,
    /// **D3 (depth-weighted tightness)** — `Σ depth_i / (1 + alts_i)`: each stall's tightness
    /// weighted by how deep it fired — a hard stall after 60 placements is felt more than one at
    /// 25. `0` with no bottleneck. Higher = harder.
    pub depth_tight_integral: f64,
    /// **D4 (payoff jaggedness)** — number of dry→payoff transitions over the harder ladder: how
    /// often a dry grind was finally broken by a placement. A stop-start solve (stall, unlock,
    /// re-stall) reads harder than one smooth descent. `0` for a single firing that pays off at
    /// once. ([`longest_dry_run`](Self::longest_dry_run) is the other half of the §4 D4 shape.)
    pub payoff_transitions: u32,

    // --- Tier E: spotting-cost signals (`docs/grader-continuous-scoring.md` §5) -----------
    /// **E1 (camo)** — near-miss camouflage at the *tightest* bottleneck: the number of
    /// pattern-shaped look-alikes the solver examines and rejects there (`examined - alts`, the
    /// structurally-valid-but-dead instances of [`GradeStep`](crate::solve::GradeStep)). The
    /// human *spotting* cost the deduction signals miss: many look-alikes per real pattern = a
    /// harder needle hunt. Stays live where `alts`/`elims` is structurally pinned (the wings
    /// always fire ~1 productive pattern, but the dead-wing count genuinely varies) — the
    /// principled cure for the wing residual. `0` with no bottleneck. Higher = harder.
    pub camo: u32,
}

/// The relative weights of the four signals in the combined order. Rough by design — the
/// inputs are already rank-normalized and we only need an ordering, so no human-timing
/// calibration is required. The priority is fixed (dry runs lead); the exact magnitudes
/// are a tuning knob (an open decision in the plan).
#[derive(Clone, Copy, Debug)]
pub struct Weights {
    /// Signal 2 (dry runs) — the primary signal, highest weight.
    pub dry: f64,
    /// Signal 1 (bottleneck count).
    pub count: f64,
    /// Signal 3 (scarcity).
    pub scarcity: f64,
    /// Signal 4 (scan work) — lowest weight.
    pub scan: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Weights { dry: 0.40, count: 0.25, scarcity: 0.20, scan: 0.15 }
    }
}

/// One puzzle's grade: its [`Signals`], the combined hardness score (a weighted sum of
/// per-signal hardness ranks in roughly `0..1`), and the band it was cut into (`0` =
/// gentlest, `n_bands - 1` = spiciest).
#[derive(Clone, Copy, Debug)]
pub struct Graded {
    pub signals: Signals,
    pub score: f64,
    pub band: usize,
}

/// The kinds whose firing counts as a **bottleneck** for `spec`: the forced technique(s).
/// A single `force` contributes its one kind; a `force_any` disjunction contributes its
/// whole set (any member firing is the node's forced deduction).
pub fn bottleneck_mask(spec: &Spec) -> KindMask {
    spec.forced_mask() | spec.force_any_set().map_or(0, |ra| ra.kinds)
}

/// Read the four [`Signals`] off one puzzle's [`GradeTrace`], given the node's
/// `bottleneck` kind mask (see [`bottleneck_mask`]). A *dry* step is one whose following
/// cheap closure placed no cell; the dry-run length is counted over consecutive harder
/// steps of any kind (a no-reward grind is felt regardless of which hard technique it
/// is), while the dry-firing count and scarcity are scoped to the bottleneck kinds.
pub fn signals_of(trace: &GradeTrace, bottleneck: KindMask) -> Signals {
    let mut s = Signals { scarcity: u32::MAX, ..Signals::default() };
    let mut cur_run = 0u32;
    let mut seen_bottleneck = false;
    for step in &trace.steps {
        let is_bottleneck = bottleneck & (1 << step.kind) != 0;
        if is_bottleneck {
            s.bottleneck_count += 1;
            s.elim_sum = s.elim_sum.saturating_add(step.elims as u32);
            // depth read at the FIRST bottleneck firing.
            if !seen_bottleneck {
                s.depth = step.depth as u32;
                seen_bottleneck = true;
            }
            // `open`/`cascade`/`alts`/`camo` (S5/S8/S7/E1) are read at the TIGHTEST bottleneck —
            // the same fewest-elim firing `scarcity` is taken over (ties keep the first such
            // firing). `camo` = the dead near-misses (`examined - alts`) at that stall.
            if (step.elims as u32) < s.scarcity {
                s.scarcity = step.elims as u32;
                s.open = step.open;
                s.cascade = step.cascade as u32;
                s.alts = step.alts;
                s.camo = step.examined.saturating_sub(step.alts);
            }
            // Tier D trajectory integrals — accumulated over EVERY bottleneck firing, not just
            // the tightest. Tightness `1/(1+alts)` is in `(0, 1]`; depth-weighted likewise scaled
            // by the fill-depth. (`open_sum` mean is taken later in `raw_granular`.)
            let tightness = 1.0 / (1.0 + step.alts as f64);
            s.tight_integral += tightness;
            s.open_sum = s.open_sum.saturating_add(step.open);
            s.depth_tight_integral += step.depth as f64 * tightness;
        }
        if step.paid_off {
            // A dry grind (>= 1 consecutive no-reward harder step) was just broken by a
            // placement: a stop-start transition (D4). Scoped to the harder ladder, like the
            // dry-run length, since the grind is felt regardless of which technique caused it.
            if cur_run > 0 {
                s.payoff_transitions += 1;
            }
            cur_run = 0;
        } else {
            cur_run += 1;
            s.longest_dry_run = s.longest_dry_run.max(cur_run);
            if is_bottleneck {
                s.dry_firings += 1;
            }
        }
    }
    s.scan_work = trace.counts[NAKED_PAIR..NUM].iter().map(|&c| c as u32).sum();
    s
}

/// Fractional percentile ranks of `values`, ties sharing the mid-rank: each element maps
/// to `(count(< v) + 0.5 * count(== v)) / n`, in `(0, 1)`. The rank-normalization step —
/// it puts signals on different scales onto one comparable axis. `0.5` for a singleton.
fn percentile_ranks(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| values[a].partial_cmp(&values[b]).unwrap_or(Ordering::Equal));
    let mut ranks = vec![0.0; n];
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && values[order[j]] == values[order[i]] {
            j += 1;
        }
        // Positions i..j are tied: percentile = (strictly-less + half the ties) / n.
        let p = (i as f64 + 0.5 * (j - i) as f64) / n as f64;
        for &k in &order[i..j] {
            ranks[k] = p;
        }
        i = j;
    }
    ranks
}

/// Grade a node's batch of [`GradeTrace`]s into bands. Computes the four [`Signals`] per
/// puzzle, rank-normalizes each across the batch (orienting so higher rank = harder),
/// weighted-sums them per [`Weights`], and quantile-cuts the combined order into `n_bands`
/// roughly equal-count bands (`0` = gentlest). The returned [`Graded`] vector is aligned
/// to the input order. A single signal that is constant across the batch contributes a
/// flat `0.5` to every score (it cannot discriminate), so the order falls to the signals
/// that do vary — exactly the relative behaviour the per-node grader wants.
pub fn grade_batch(
    traces: &[GradeTrace],
    bottleneck: KindMask,
    weights: &Weights,
    n_bands: usize,
) -> Vec<Graded> {
    let signals: Vec<Signals> = traces.iter().map(|t| signals_of(t, bottleneck)).collect();
    grade_signals(&signals, weights, n_bands)
}

/// The relative grade over a batch of already-computed [`Signals`] — the meat of
/// [`grade_batch`], split out so callers that already hold the signals (the calibration
/// harness, which reads them off a cache) can compute the same relative score/band without
/// re-solving. Rank-normalizes each signal across the batch, weighted-sums per [`Weights`],
/// and quantile-cuts into `n_bands`. Aligned to the input order.
pub fn grade_signals(signals: &[Signals], weights: &Weights, n_bands: usize) -> Vec<Graded> {
    let n_bands = n_bands.max(1);
    let n = signals.len();
    if n == 0 {
        return Vec::new();
    }

    // Per-signal hardness ranks. The two dry components are averaged into one "dry" rank
    // so signal 2 stays a single weighted term; scarcity is inverted (lower = harder).
    let count_r = percentile_ranks(&col(signals, |s| s.bottleneck_count as f64));
    let dryf_r = percentile_ranks(&col(signals, |s| s.dry_firings as f64));
    let dryr_r = percentile_ranks(&col(signals, |s| s.longest_dry_run as f64));
    let scar_r = percentile_ranks(&col(signals, |s| s.scarcity as f64));
    let scan_r = percentile_ranks(&col(signals, |s| s.scan_work as f64));

    let mut graded: Vec<Graded> = (0..n)
        .map(|i| {
            let dry_rank = 0.5 * (dryf_r[i] + dryr_r[i]);
            let score = weights.dry * dry_rank
                + weights.count * count_r[i]
                + weights.scarcity * (1.0 - scar_r[i]) // scarcity: lower min-elims = harder
                + weights.scan * scan_r[i];
            Graded { signals: signals[i], score, band: 0 }
        })
        .collect();

    // Quantile-cut: order by score and assign equal-count bands by position. Exact score
    // ties at a band boundary fall by position (rough is acceptable per the plan).
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| graded[a].score.partial_cmp(&graded[b].score).unwrap_or(Ordering::Equal));
    for (pos, &i) in order.iter().enumerate() {
        graded[i].band = (pos * n_bands / n).min(n_bands - 1);
    }
    graded
}

/// Extract one signal as an `f64` column over the batch.
fn col(signals: &[Signals], f: impl Fn(&Signals) -> f64) -> Vec<f64> {
    signals.iter().map(f).collect()
}

/// Grade a node's yielded `puzzles` end to end: solve each with `spec`'s baseline toolbox
/// (the instrumented [`solve_graded`]), then [`grade_batch`] over the node's bottleneck
/// kinds. The grading solve runs on the production row-major banded board. Cold path:
/// grading happens once per yielded puzzle, off the hot generation loop.
pub fn grade_node(
    spec: &Spec,
    puzzles: &[DigitGrid],
    weights: &Weights,
    n_bands: usize,
) -> Vec<Graded> {
    let baseline = spec.baseline_mask();
    let bottleneck = bottleneck_mask(spec);
    let traces: Vec<GradeTrace> = puzzles
        .iter()
        .map(|g| solve_graded(&SolverState::<Bands<RowMajor>>::from_digits(g), baseline))
        .collect();
    grade_batch(&traces, bottleneck, weights, n_bands)
}

/// A single puzzle's **absolute** combined hardness score, the input the per-technique band
/// cut ([`band_calibrated`]) operates on. Where [`grade_batch`] rank-normalizes a whole
/// node's batch against itself, this maps one puzzle to a stable float with no batch, for
/// callers that grade one puzzle at a time (the web app).
///
/// It is built so the band stays a *sub-tier* read — "how hard is this puzzle **for its
/// technique**". The integer **grind** term (the longest dry run plus how many *extra* times
/// beyond the first the forced technique fired) is the primary axis, exactly as in the
/// relative grader. But for the cleanly single-forced UI specs that grind is almost always
/// `0` (the forced technique fires once and a single immediately follows), so it is fused
/// with a fractional **scarcity pressure** in `[0, 1)`: the fewer candidates the tightest
/// bottleneck firing eliminated, the harder it was to spot. Kept strictly below `1`, scarcity
/// only ever orders puzzles *within* the same grind level — which is what discriminates the
/// fish nodes (X-Wing/Swordfish/Jellyfish), where grind never varies and scarcity is the only
/// live signal. A puzzle with no bottleneck (a trunk-forced / Beginner node, no harder phase)
/// scores `0` — uniformly gentle.
pub fn hardness_score(s: &Signals) -> f64 {
    if s.bottleneck_count == 0 {
        return 0.0;
    }
    let grind = (s.longest_dry_run + (s.bottleneck_count - 1)) as f64;
    // Scarcity pressure: lower min-elims = a tighter, harder stall. `1/(1+scarcity)` is in
    // `(0, 0.5]` for any real firing (>= 1 elim), so it sub-orders within a grind level
    // without ever bumping a puzzle into the next one. (When bottleneck_count > 0 scarcity is
    // always finite; the `min` is just a guard against the no-firing sentinel.)
    let pressure = 1.0 / (1.0 + s.scarcity.min(u16::MAX as u32) as f64);
    grind + pressure
}

// --- granular per-branch score (docs/grader-granular-scoring.md, Tier A) ------------------
//
// [`hardness_score`] is too quantized to band cleanly on the branch-pinned techniques: where a
// technique's elimination count is structurally fixed (every wing kills ~1 candidate) the score
// lands on a handful of values and the even-thirds cut collapses (xyz-wing: p33 == p66, medium
// band empty). The cure is a continuous sub-order term: the integer `grind` stays the dominant
// axis, but within a grind level puzzles are spread by the *stall-state* signals (`open`,
// `cascade`, `depth`, mean `elims`) that stay live even when `elims` is pinned. Each is mapped
// through a per-technique logistic so the blend is a weighted average in `(0, 1)` — strictly
// below 1, so it never crosses a grind step. Mean/scale/sign are calibration DATA
// ([`TechNorm::calibrate`] over a mined corpus), exactly as `THRESHOLDS` is.

/// A logistic squash to `(0, 1)`: maps a z-score to a smooth, monotone, bounded rank proxy.
fn logistic(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

/// Minimum |rank correlation| with the reference for a signal to earn a place in the blend. Below
/// this its orientation is within sampling noise of zero and flips between calibration samples,
/// which reorders the score and destabilizes the rating; such signals are dropped (weight 0). Set
/// comfortably above the ~`1/sqrt(n)` noise of the ~75-puzzle split-half calibrations the
/// stability check uses (≈0.12), so a kept signal has a *determined* direction. Every signal's
/// orientation is data-driven (the spec's §8: do not assume direction) — this only gates whether
/// the direction is real enough to act on. See [`TechNorm::calibrate`]. Also the Stage-3
/// per-signal gate: a signal is re-oriented from the human label only when its human correlation
/// clears this floor ([`TechNorm::reoriented`]).
pub const CORR_FLOOR: f64 = 0.20;

/// Per-signal normalization: a calibrated `(mean, scale)` z-score, an orientation `sign`
/// (`+1` if a higher raw value is *harder* for this technique, `-1` if lower is), and the
/// branch `weight` this signal carries in the blend. A signal that is constant within a
/// technique (`scale` ~ 0) gets `weight == 0` so it cannot pull the blend toward `0.5`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SigNorm {
    pub mean: f64,
    pub scale: f64,
    pub sign: f64,
    pub weight: f64,
}

impl SigNorm {
    /// The signal's logistic contribution for raw value `x` (in `(0, 1)`), oriented so
    /// higher = harder.
    fn rank(&self, x: f64) -> f64 {
        logistic(self.sign * (x - self.mean) / self.scale)
    }
}

/// The per-technique calibration for the granular signals: the 5 tightest-stall ones (`open` S5,
/// `cascade` S8, `depth` S6, mean `elims` S3', the Tier-B honest scarcity `alts` S7), the 4
/// Tier-D trajectory features (`tight`, `open_mean`, `depth_tight`, `transitions`), and the
/// Tier-E spotting signal (`camo`, E1 near-miss camouflage). Built once over a mined corpus
/// ([`calibrate`](Self::calibrate)) and baked, then read by [`granular_score`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TechNorm {
    pub open: SigNorm,
    pub cascade: SigNorm,
    pub depth: SigNorm,
    pub elim: SigNorm,
    pub alts: SigNorm,
    // Tier D trajectory signals (see [`Signals`]): `tight` = D1 `Σ 1/(1+alts)`, `open_mean` = D2
    // mean `open`, `depth_tight` = D3 depth-weighted tightness, `transitions` = D4 jaggedness.
    pub tight: SigNorm,
    pub open_mean: SigNorm,
    pub depth_tight: SigNorm,
    pub transitions: SigNorm,
    // Tier E spotting signal (see [`Signals`]): `camo` = E1 near-miss camouflage.
    pub camo: SigNorm,
}

/// The number of signals a [`TechNorm`] blends: the 5 tightest-stall signals (Tier A/B), the 4
/// Tier-D trajectory features, and the Tier-E spotting signal. `raw_granular`/`branch_weights`/
/// [`TechNorm`] all walk in this order: open, cascade, depth, mean-elims, alts, tight (D1),
/// mean-open (D2), depth-tight (D3), transitions (D4), camo (E1).
const NSIG: usize = 10;

/// The [`NSIG`] granular-signal names, in [`raw_granular`] / [`branch_weights`] / [`TechNorm`]
/// order. For the Stage-3 orientation report (so a flipped sign is named, not an index).
pub const SIGNAL_NAMES: [&str; NSIG] =
    ["open", "cascade", "depth", "elim", "alts", "tight", "open_mean", "depth_tight", "transitions", "camo"];

/// The raw granular signal values of `s`, in the [`TechNorm`] field order. The means collapse to
/// the single firing's value for the single-forced UI specs (`elim_mean == scarcity`,
/// `open_mean == open`, `tight == 1/(1+alts)`), so the trajectory features only diverge from the
/// tightest-stall ones on the multi-firing nodes.
fn raw_granular(s: &Signals) -> [f64; NSIG] {
    let n = s.bottleneck_count;
    let elim_mean = if n > 0 { s.elim_sum as f64 / n as f64 } else { 0.0 };
    let open_mean = if n > 0 { s.open_sum as f64 / n as f64 } else { 0.0 };
    [
        s.open as f64,
        s.cascade as f64,
        s.depth as f64,
        elim_mean,
        s.alts as f64,
        s.tight_integral,
        open_mean,
        s.depth_tight_integral,
        s.payoff_transitions as f64,
        s.camo as f64,
    ]
}

/// The base branch weights of the granular signals, in [`raw_granular`] order. Priority is
/// fixed per branch (the spec's §5 table); calibration only sets each signal's mean/scale/sign,
/// never these. The 5 tightest-stall signals: every branch leans on `alts` (the honest scarcity)
/// — `Subset` pairs it with mean-`elims` (real elim spread, several firings); `Fish` pairs it with
/// `open` (grind ≡ 0, elims secondary); `Bivalue` leads on it with `open` (its `elims` is pinned
/// ~1, so `alts` is the only live scarcity). The 4 Tier-D trajectory features are weighted ONLY
/// for `Subset` (the most multi-firing branch, where the measured win lands); `Fish` is ~always
/// single-firing and `Bivalue`'s degenerate reference makes them unstable, so both keep them at 0
/// and score exactly as before Tier D (see [`Self`](branch_weights) body + the doc §8.3). The
/// Tier-E spotting signal `camo` is weighted ONLY for `Bivalue`: it is the principled cure for the
/// wing residual (the deduction signals are pinned there, but the dead-look-alike count varies and
/// is reproducible), and the subsets/fish already PASS, so they keep it 0 and score as before.
fn branch_weights(branch: Branch) -> [f64; NSIG] {
    match branch {
        // tightest-stall (Tier A/B):          | Tier-D trajectory:                    | Tier-E:
        // open  cascade depth elim  alts       | tight open_mean depth_tight transitions| camo
        //
        // Subset fires the bottleneck most (subset drills 30-38% multi), so it leans hardest on
        // the trajectory features; `tight` (the multi-stall generalisation of `alts`) and the
        // depth-weighted integral carry the within-node multi-firing structure the tightest-stall
        // reduction throws away.
        Branch::Subset => [0.20, 0.05, 0.05, 0.35, 0.30, 0.10, 0.05, 0.05, 0.05, 0.00],
        // Fish is ~always single-firing (1-4% multi), so the trajectory features reduce to the
        // tightest-stall ones; their weight is left at 0 so Fish scores EXACTLY as before Tier D.
        Branch::Fish => [0.15, 0.05, 0.05, 0.45, 0.30, 0.00, 0.00, 0.00, 0.00, 0.00],
        // Wings multi-fire 13-31%, BUT their relative reference is itself near-degenerate (`elim`
        // pinned ~1, the known wing residual), so the deduction-cost trajectory (Tier D) is
        // withheld here. The Tier-E spotting signal `camo` is NOT a branch base — it destabilises
        // the wings that already pass §2.3, so it is applied per technique via [`camo_weight`]
        // (xyz-wing only); the branch base leaves it 0. The rest stay as Tier A/B set them.
        Branch::Bivalue => [0.30, 0.15, 0.10, 0.00, 0.45, 0.00, 0.00, 0.00, 0.00, 0.00],
        Branch::Trunk => [0.0; NSIG], // never keys the score (no bottleneck)
    }
}

/// The index of the Tier-E `camo` signal in the [`raw_granular`] / [`branch_weights`] vector.
const CAMO_IDX: usize = 9;

/// Per-technique **E1 camouflage** weight, overriding the [`branch_weights`] base at [`CAMO_IDX`].
/// Camo is a *spotting*-cost signal, and the deduction signals already clear the §2.3 split-half
/// stability gate for every technique except **xyz-wing** (0.85 < 0.90 — the residual, exactly as
/// it was the gate for the 3-band cut). Camo cures it (0.85 → 0.92 at weight 0.40) because the
/// dead-look-alike count is reproducible where `alts`/`elims` is pinned. It is withheld everywhere
/// else: xy-wing (split-half 1.00) and w-wing (0.90, its camo is strongly train/drill-split)
/// already pass and camo only destabilises them, and the subsets/fish never needed it — so all
/// keep camo 0 and score byte-identical to pre-E. (This is the one per-technique weight override
/// the spec's Tier-F1 systematises; the granular doc set the precedent with the Fish elim override.)
fn camo_weight(kind: usize) -> f64 {
    match kind {
        XYZ_WING => 0.40,
        _ => 0.0,
    }
}

impl TechNorm {
    /// The blended continuous sub-order in `(0, 1)`: the weight-normalized average of the
    /// signals' logistic ranks. Strictly `< 1` (every term is `< 1` and the weights sum to
    /// what they sum to), so [`granular_score`] never lets it cross a `grind` step. Falls back
    /// to `0.5` only if every signal was constant (no live weight) — a fully degenerate node.
    fn blend(&self, s: &Signals) -> f64 {
        let raw = raw_granular(s);
        let sigs = [
            &self.open,
            &self.cascade,
            &self.depth,
            &self.elim,
            &self.alts,
            &self.tight,
            &self.open_mean,
            &self.depth_tight,
            &self.transitions,
            &self.camo,
        ];
        let mut acc = 0.0;
        let mut wsum = 0.0;
        for (sig, &x) in sigs.iter().zip(raw.iter()) {
            if sig.weight > 0.0 {
                acc += sig.weight * sig.rank(x);
                wsum += sig.weight;
            }
        }
        if wsum > 0.0 { acc / wsum } else { 0.5 }
    }

    /// Calibrate the signals for technique `kind` from a corpus `sample` and the matching
    /// relative-grade reference `rel` (e.g. [`grade_signals`] scores over the same sample):
    /// each signal's `(mean, scale)` is the sample mean/stddev, its `sign` is the sign of its
    /// (rank) correlation with `rel` (so higher normalized = harder, matching the relative
    /// grader), and its `weight` is the [`branch_weights`] base for `kind`'s branch — with the
    /// per-technique [`camo_weight`] override — zeroed when the signal is constant or its
    /// orientation is undetermined. `rel` must be aligned to `sample`.
    pub fn calibrate(kind: usize, sample: &[Signals], rel: &[f64]) -> TechNorm {
        let mut w = branch_weights(branch_of(kind));
        w[CAMO_IDX] = camo_weight(kind);
        let cols: [Vec<f64>; NSIG] = {
            let mut c: [Vec<f64>; NSIG] = core::array::from_fn(|_| Vec::new());
            for s in sample {
                let raw = raw_granular(s);
                for k in 0..NSIG {
                    c[k].push(raw[k]);
                }
            }
            c
        };
        let build = |k: usize| -> SigNorm {
            let (mean, sd) = mean_std(&cols[k]);
            let corr = rank_corr(&cols[k], rel);
            // Keep a signal only if it varies AND its orientation is *determined* — its
            // correlation with the reference clears the noise floor. A signal below the floor has
            // a sign that flips between calibration samples (it reorders the score and
            // destabilizes the rating), so it is dropped (weight 0).
            let live = sd >= 1e-9 && corr.abs() >= CORR_FLOOR;
            let sign = if corr >= 0.0 { 1.0 } else { -1.0 };
            SigNorm { mean, scale: sd.max(1e-9), sign, weight: if live { w[k] } else { 0.0 } }
        };
        TechNorm {
            open: build(0),
            cascade: build(1),
            depth: build(2),
            elim: build(3),
            alts: build(4),
            tight: build(5),
            open_mean: build(6),
            depth_tight: build(7),
            transitions: build(8),
            camo: build(9),
        }
    }

    /// Stage 4 (`docs/grader-external-calibration.md` §5): a copy of `self` with each signal's
    /// `(mean, scale)` recomputed from a **natural-puzzle** `sample`, inheriting the existing
    /// `sign`/`weight` (the Stage-3-adjudicated orientation, the branch weight). The §4.3.2
    /// distribution-shift cure at the per-signal level: the isolated mean/scale can push a natural
    /// signal into a saturated logistic tail (every natural value ~0 or ~1, no discrimination), and
    /// re-centering on the natural distribution restores its spread. **Re-means, it does not
    /// re-orient or re-weight** (signs are Stage 3, weights are the optional re-weight). A signal that
    /// is constant across `sample` (`scale ~ 0`) keeps its baked params — there is nothing to
    /// re-center, and a zero scale would divide by ~0.
    pub fn remeaned(&self, sample: &[Signals]) -> TechNorm {
        let cols: [Vec<f64>; NSIG] = {
            let mut c: [Vec<f64>; NSIG] = core::array::from_fn(|_| Vec::with_capacity(sample.len()));
            for s in sample {
                let raw = raw_granular(s);
                for k in 0..NSIG {
                    c[k].push(raw[k]);
                }
            }
            c
        };
        let base = [
            self.open, self.cascade, self.depth, self.elim, self.alts,
            self.tight, self.open_mean, self.depth_tight, self.transitions, self.camo,
        ];
        let remean = |k: usize| -> SigNorm {
            let (mean, sd) = mean_std(&cols[k]);
            if sd >= 1e-9 { SigNorm { mean, scale: sd, ..base[k] } } else { base[k] }
        };
        TechNorm {
            open: remean(0),
            cascade: remean(1),
            depth: remean(2),
            elim: remean(3),
            alts: remean(4),
            tight: remean(5),
            open_mean: remean(6),
            depth_tight: remean(7),
            transitions: remean(8),
            camo: remean(9),
        }
    }

    /// Stage 4 (the *optional* per-technique re-weight): a copy of `self` with each signal's blend
    /// `weight` replaced by `w[j]` (in [`raw_granular`] order), keeping mean/scale/sign. The search
    /// target of the held-out weight fit; the blend renormalizes by the live weight sum, so only the
    /// relative magnitudes matter. Baked only where the fitted weights beat the re-mine on a held-out
    /// bucket half (criterion §6.4) — most buckets are too thin and the gate keeps the baked weights.
    pub fn reweighted(&self, w: &[f64; NSIG]) -> TechNorm {
        let set = |sig: SigNorm, weight: f64| SigNorm { weight, ..sig };
        TechNorm {
            open: set(self.open, w[0]),
            cascade: set(self.cascade, w[1]),
            depth: set(self.depth, w[2]),
            elim: set(self.elim, w[3]),
            alts: set(self.alts, w[4]),
            tight: set(self.tight, w[5]),
            open_mean: set(self.open_mean, w[6]),
            depth_tight: set(self.depth_tight, w[7]),
            transitions: set(self.transitions, w[8]),
            camo: set(self.camo, w[9]),
        }
    }
}

/// One puzzle's **granular absolute** score: the integer `grind` backbone plus the continuous
/// `(0, 1)` sub-order blended off the stall signals via the technique's [`TechNorm`]. Same
/// shape and same dominant axis as [`hardness_score`] — `grind` still leads wherever it varies
/// — but the sub-order is now genuinely continuous, so the even-thirds cut no longer collapses
/// on the branch-pinned techniques (xyz-wing/fish). A puzzle with no bottleneck scores `0`.
pub fn granular_score(s: &Signals, norm: &TechNorm) -> f64 {
    if s.bottleneck_count == 0 {
        return 0.0;
    }
    let grind = (s.longest_dry_run + (s.bottleneck_count - 1)) as f64;
    grind + norm.blend(s)
}

// --- Tier C: continuous within-technique rating (docs/grader-continuous-scoring.md) ---------
//
// The 3-band cut discarded everything finer than thirds. Tier C keeps the full resolution of the
// continuous [`granular_score`] by mapping it through a per-technique **CDF**: a small monotone
// breakpoint table (the score at 0/10/.../100 percentiles of the calibration pool). The puzzle's
// `rating` is its percentile within its technique, in `[0, 1]` — a continuous read. Because the
// rating is (by construction) ~uniform on the calibration distribution, cutting it into `n` equal
// slices ([`band_of_rating`]) yields ~even n-tiles for ANY `n`, so the band count is now a runtime
// choice (the UI takes 5), not a per-n calibration table.

/// Number of CDF breakpoints baked per technique: the score at percentiles 0, 10, …, 100.
pub const CDF_ANCHORS: usize = 11;

/// The per-technique CDF breakpoints of `scores`: anchor `i` is the `i/10` quantile (so
/// `[0]` = min, `[10]` = max), monotone non-decreasing. The inverse of [`rating_from_cdf`];
/// baked into [`GRANULAR_CDF`] and recomputed by the calibration workbench from the same pool.
pub fn cdf_of(scores: &[f64]) -> [f64; CDF_ANCHORS] {
    let mut v = scores.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let mut anchors = [0.0; CDF_ANCHORS];
    if v.is_empty() {
        return anchors;
    }
    for (i, a) in anchors.iter_mut().enumerate() {
        let p = i as f64 / (CDF_ANCHORS - 1) as f64;
        *a = v[((p * v.len() as f64) as usize).min(v.len() - 1)];
    }
    anchors
}

/// Map a raw [`granular_score`] to its `[0, 1]` percentile against a technique's `cdf`
/// breakpoints (piecewise-linear inverse-CDF). Clamps below the min to `0.0` and above the max
/// to `1.0`; flat CDF segments (ties) contribute no width. Monotone non-decreasing in `score`.
pub fn rating_from_cdf(score: f64, cdf: &[f64; CDF_ANCHORS]) -> f64 {
    let last = CDF_ANCHORS - 1;
    if score <= cdf[0] {
        return 0.0;
    }
    if score >= cdf[last] {
        return 1.0;
    }
    for i in 0..last {
        if score < cdf[i + 1] {
            let span = cdf[i + 1] - cdf[i];
            let frac = if span > 0.0 { (score - cdf[i]) / span } else { 0.0 };
            return (i as f64 + frac) / last as f64;
        }
    }
    1.0
}

/// Cut a `[0, 1]` rating into one of `n_bands` equal slices (`0` = gentlest,
/// `n_bands - 1` = spiciest). `floor(rating · n)`, clamped. Even n-tiles for any `n` because the
/// rating is a per-technique percentile.
pub fn band_of_rating(rating: f64, n_bands: usize) -> usize {
    let n = n_bands.max(1);
    ((rating * n as f64) as usize).min(n - 1)
}

// --- BAKED granular calibration (regenerate via `grade_diag --calibrate`) ------------------
//
// Per-technique [`TechNorm`] (signal mean/scale/sign/weight) and the granular-score CDF the
// [`rating`] reads against, mined over the pooled train+drill corpus. CALIBRATED, not derived —
// rerun `examples/grade_diag --calibrate` and repaste both arrays if the generator or the grading
// solve changes. Trunk rows (kinds < `NAKED_PAIR`) are unused placeholders: a trunk-only spec has
// no bottleneck key, so [`rating`] short-circuits to `0` before indexing these.
//
// STAGE 3 (docs/grader-external-calibration.md §5): a few **signs** are human-oriented, not
// grade_batch-oriented — **naked-triple** `tight`/`depth_tight` (+1 -> -1) and **xy-wing** `cascade`
// (-1 -> +1), with their CDFs re-derived under the flipped sign (`grade_diag --human-orient` surgical
// bake). Mean/scale/weight are untouched (re-orient, not re-weight). The stability gate reverted the
// naked-pair / w-wing flips the human data also suggested (they regressed split-half stability < 0.90).

pub const GRANULAR_NORM: [TechNorm; NUM] = [
    /* naked-single   */ TechNorm { open: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, cascade: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, depth: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, elim: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, alts: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, tight: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, open_mean: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, depth_tight: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, transitions: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, camo: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 } },
    /* hidden-single  */ TechNorm { open: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, cascade: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, depth: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, elim: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, alts: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, tight: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, open_mean: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, depth_tight: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, transitions: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, camo: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 } },
    /* lc-pointing    */ TechNorm { open: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, cascade: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, depth: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, elim: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, alts: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, tight: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, open_mean: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, depth_tight: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, transitions: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, camo: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 } },
    /* lc-claiming    */ TechNorm { open: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, cascade: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, depth: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, elim: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, alts: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, tight: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, open_mean: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, depth_tight: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, transitions: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 }, camo: SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 } },
    /* naked-pair     */ TechNorm { open: SigNorm { mean: 122.020, scale: 37.346, sign: -1.0, weight: 0.20 }, cascade: SigNorm { mean: 32.027, scale: 18.211, sign: -1.0, weight: 0.05 }, depth: SigNorm { mean: 40.180, scale: 8.299, sign: 1.0, weight: 0.00 }, elim: SigNorm { mean: 3.427, scale: 1.431, sign: -1.0, weight: 0.35 }, alts: SigNorm { mean: 2.180, scale: 1.084, sign: 1.0, weight: 0.30 }, tight: SigNorm { mean: 0.471, scale: 0.242, sign: 1.0, weight: 0.10 }, open_mean: SigNorm { mean: 121.508, scale: 36.748, sign: -1.0, weight: 0.05 }, depth_tight: SigNorm { mean: 19.393, scale: 11.321, sign: 1.0, weight: 0.05 }, transitions: SigNorm { mean: 0.167, scale: 0.373, sign: 1.0, weight: 0.05 }, camo: SigNorm { mean: 4.120, scale: 3.568, sign: 1.0, weight: 0.00 } },
    /* hidden-pair    */ TechNorm { open: SigNorm { mean: 133.503, scale: 33.937, sign: -1.0, weight: 0.00 }, cascade: SigNorm { mean: 32.980, scale: 18.853, sign: -1.0, weight: 0.05 }, depth: SigNorm { mean: 37.057, scale: 7.101, sign: -1.0, weight: 0.00 }, elim: SigNorm { mean: 3.802, scale: 1.454, sign: -1.0, weight: 0.35 }, alts: SigNorm { mean: 2.450, scale: 1.260, sign: 1.0, weight: 0.30 }, tight: SigNorm { mean: 0.446, scale: 0.213, sign: 1.0, weight: 0.10 }, open_mean: SigNorm { mean: 133.679, scale: 33.161, sign: -1.0, weight: 0.00 }, depth_tight: SigNorm { mean: 16.828, scale: 8.526, sign: 1.0, weight: 0.05 }, transitions: SigNorm { mean: 0.340, scale: 0.521, sign: 1.0, weight: 0.05 }, camo: SigNorm { mean: 5.727, scale: 5.799, sign: 1.0, weight: 0.00 } },
    /* naked-triple   */ TechNorm { open: SigNorm { mean: 131.797, scale: 31.252, sign: -1.0, weight: 0.20 }, cascade: SigNorm { mean: 35.393, scale: 16.992, sign: -1.0, weight: 0.05 }, depth: SigNorm { mean: 37.557, scale: 6.600, sign: 1.0, weight: 0.00 }, elim: SigNorm { mean: 4.734, scale: 1.660, sign: -1.0, weight: 0.35 }, alts: SigNorm { mean: 1.390, scale: 0.747, sign: 1.0, weight: 0.00 }, tight: SigNorm { mean: 0.559, scale: 0.241, sign: -1.0, weight: 0.10 }, open_mean: SigNorm { mean: 132.241, scale: 31.005, sign: -1.0, weight: 0.00 }, depth_tight: SigNorm { mean: 21.071, scale: 9.267, sign: -1.0, weight: 0.05 }, transitions: SigNorm { mean: 0.360, scale: 0.520, sign: 1.0, weight: 0.05 }, camo: SigNorm { mean: 8.773, scale: 8.169, sign: 1.0, weight: 0.00 } },
    /* hidden-triple  */ TechNorm { open: SigNorm { mean: 140.130, scale: 30.149, sign: -1.0, weight: 0.00 }, cascade: SigNorm { mean: 34.957, scale: 18.766, sign: -1.0, weight: 0.05 }, depth: SigNorm { mean: 35.850, scale: 5.783, sign: -1.0, weight: 0.00 }, elim: SigNorm { mean: 5.341, scale: 1.770, sign: -1.0, weight: 0.35 }, alts: SigNorm { mean: 1.303, scale: 0.609, sign: 1.0, weight: 0.00 }, tight: SigNorm { mean: 0.563, scale: 0.218, sign: 1.0, weight: 0.10 }, open_mean: SigNorm { mean: 139.855, scale: 28.716, sign: -1.0, weight: 0.00 }, depth_tight: SigNorm { mean: 20.326, scale: 8.161, sign: 1.0, weight: 0.05 }, transitions: SigNorm { mean: 0.397, scale: 0.509, sign: 1.0, weight: 0.05 }, camo: SigNorm { mean: 10.280, scale: 9.496, sign: 1.0, weight: 0.00 } },
    /* naked-quad     */ TechNorm { open: SigNorm { mean: 142.050, scale: 31.492, sign: -1.0, weight: 0.00 }, cascade: SigNorm { mean: 35.520, scale: 18.554, sign: -1.0, weight: 0.05 }, depth: SigNorm { mean: 35.397, scale: 6.152, sign: -1.0, weight: 0.00 }, elim: SigNorm { mean: 6.138, scale: 2.045, sign: -1.0, weight: 0.35 }, alts: SigNorm { mean: 1.147, scale: 0.446, sign: 1.0, weight: 0.00 }, tight: SigNorm { mean: 0.577, scale: 0.216, sign: 1.0, weight: 0.10 }, open_mean: SigNorm { mean: 142.796, scale: 30.788, sign: -1.0, weight: 0.00 }, depth_tight: SigNorm { mean: 20.428, scale: 7.888, sign: 1.0, weight: 0.05 }, transitions: SigNorm { mean: 0.430, scale: 0.521, sign: 1.0, weight: 0.05 }, camo: SigNorm { mean: 13.630, scale: 11.366, sign: 1.0, weight: 0.00 } },
    /* hidden-quad    */ TechNorm { open: SigNorm { mean: 139.457, scale: 27.912, sign: -1.0, weight: 0.20 }, cascade: SigNorm { mean: 36.773, scale: 17.365, sign: -1.0, weight: 0.05 }, depth: SigNorm { mean: 36.130, scale: 5.584, sign: 1.0, weight: 0.00 }, elim: SigNorm { mean: 7.182, scale: 2.089, sign: -1.0, weight: 0.35 }, alts: SigNorm { mean: 1.053, scale: 0.253, sign: 1.0, weight: 0.00 }, tight: SigNorm { mean: 0.567, scale: 0.172, sign: 1.0, weight: 0.10 }, open_mean: SigNorm { mean: 139.794, scale: 27.366, sign: -1.0, weight: 0.05 }, depth_tight: SigNorm { mean: 20.569, scale: 7.130, sign: 1.0, weight: 0.05 }, transitions: SigNorm { mean: 0.393, scale: 0.546, sign: 1.0, weight: 0.05 }, camo: SigNorm { mean: 16.983, scale: 14.357, sign: 1.0, weight: 0.00 } },
    /* x-wing         */ TechNorm { open: SigNorm { mean: 108.707, scale: 37.764, sign: -1.0, weight: 0.15 }, cascade: SigNorm { mean: 36.807, scale: 9.083, sign: -1.0, weight: 0.05 }, depth: SigNorm { mean: 43.913, scale: 8.577, sign: 1.0, weight: 0.05 }, elim: SigNorm { mean: 3.773, scale: 1.465, sign: -1.0, weight: 0.45 }, alts: SigNorm { mean: 1.347, scale: 0.577, sign: 1.0, weight: 0.30 }, tight: SigNorm { mean: 0.451, scale: 0.089, sign: -1.0, weight: 0.00 }, open_mean: SigNorm { mean: 108.710, scale: 37.768, sign: -1.0, weight: 0.00 }, depth_tight: SigNorm { mean: 19.642, scale: 4.924, sign: 1.0, weight: 0.00 }, transitions: SigNorm { mean: 0.013, scale: 0.115, sign: 1.0, weight: 0.00 }, camo: SigNorm { mean: 3.507, scale: 2.884, sign: 1.0, weight: 0.00 } },
    /* swordfish      */ TechNorm { open: SigNorm { mean: 110.897, scale: 31.398, sign: -1.0, weight: 0.15 }, cascade: SigNorm { mean: 37.107, scale: 8.338, sign: -1.0, weight: 0.05 }, depth: SigNorm { mean: 43.157, scale: 6.755, sign: 1.0, weight: 0.05 }, elim: SigNorm { mean: 5.308, scale: 2.062, sign: -1.0, weight: 0.45 }, alts: SigNorm { mean: 1.607, scale: 0.540, sign: 1.0, weight: 0.00 }, tight: SigNorm { mean: 0.407, scale: 0.091, sign: 1.0, weight: 0.00 }, open_mean: SigNorm { mean: 110.855, scale: 31.385, sign: -1.0, weight: 0.00 }, depth_tight: SigNorm { mean: 17.649, scale: 4.953, sign: 1.0, weight: 0.00 }, transitions: SigNorm { mean: 0.033, scale: 0.180, sign: 1.0, weight: 0.00 }, camo: SigNorm { mean: 4.153, scale: 3.459, sign: -1.0, weight: 0.00 } },
    /* jellyfish      */ TechNorm { open: SigNorm { mean: 111.217, scale: 23.297, sign: -1.0, weight: 0.15 }, cascade: SigNorm { mean: 37.453, scale: 6.036, sign: -1.0, weight: 0.05 }, depth: SigNorm { mean: 43.173, scale: 4.757, sign: 1.0, weight: 0.05 }, elim: SigNorm { mean: 7.095, scale: 2.119, sign: -1.0, weight: 0.45 }, alts: SigNorm { mean: 1.947, scale: 0.265, sign: 1.0, weight: 0.00 }, tight: SigNorm { mean: 0.349, scale: 0.060, sign: 1.0, weight: 0.00 }, open_mean: SigNorm { mean: 111.283, scale: 23.278, sign: -1.0, weight: 0.00 }, depth_tight: SigNorm { mean: 15.100, scale: 3.145, sign: 1.0, weight: 0.00 }, transitions: SigNorm { mean: 0.090, scale: 0.286, sign: 1.0, weight: 0.00 }, camo: SigNorm { mean: 5.520, scale: 4.524, sign: 1.0, weight: 0.00 } },
    /* xy-wing        */ TechNorm { open: SigNorm { mean: 75.327, scale: 30.878, sign: -1.0, weight: 0.00 }, cascade: SigNorm { mean: 24.660, scale: 12.449, sign: 1.0, weight: 0.15 }, depth: SigNorm { mean: 51.000, scale: 8.955, sign: -1.0, weight: 0.00 }, elim: SigNorm { mean: 1.668, scale: 0.731, sign: -1.0, weight: 0.00 }, alts: SigNorm { mean: 1.773, scale: 1.108, sign: 1.0, weight: 0.45 }, tight: SigNorm { mean: 0.528, scale: 0.281, sign: 1.0, weight: 0.00 }, open_mean: SigNorm { mean: 75.446, scale: 30.059, sign: 1.0, weight: 0.00 }, depth_tight: SigNorm { mean: 26.652, scale: 13.169, sign: 1.0, weight: 0.00 }, transitions: SigNorm { mean: 0.127, scale: 0.352, sign: 1.0, weight: 0.00 }, camo: SigNorm { mean: 7.120, scale: 5.552, sign: 1.0, weight: 0.00 } },
    /* xyz-wing       */ TechNorm { open: SigNorm { mean: 82.563, scale: 29.472, sign: 1.0, weight: 0.00 }, cascade: SigNorm { mean: 25.033, scale: 13.867, sign: -1.0, weight: 0.15 }, depth: SigNorm { mean: 49.467, scale: 8.071, sign: -1.0, weight: 0.00 }, elim: SigNorm { mean: 1.063, scale: 0.225, sign: -1.0, weight: 0.00 }, alts: SigNorm { mean: 1.360, scale: 0.563, sign: 1.0, weight: 0.45 }, tight: SigNorm { mean: 0.511, scale: 0.166, sign: 1.0, weight: 0.00 }, open_mean: SigNorm { mean: 82.055, scale: 29.489, sign: 1.0, weight: 0.00 }, depth_tight: SigNorm { mean: 25.312, scale: 9.103, sign: 1.0, weight: 0.00 }, transitions: SigNorm { mean: 0.253, scale: 0.450, sign: 1.0, weight: 0.00 }, camo: SigNorm { mean: 6.070, scale: 4.650, sign: 1.0, weight: 0.40 } },
    /* w-wing         */ TechNorm { open: SigNorm { mean: 73.240, scale: 30.810, sign: -1.0, weight: 0.00 }, cascade: SigNorm { mean: 22.067, scale: 13.296, sign: -1.0, weight: 0.15 }, depth: SigNorm { mean: 52.177, scale: 8.884, sign: -1.0, weight: 0.00 }, elim: SigNorm { mean: 1.639, scale: 0.813, sign: -1.0, weight: 0.00 }, alts: SigNorm { mean: 3.313, scale: 2.809, sign: 1.0, weight: 0.00 }, tight: SigNorm { mean: 0.425, scale: 0.264, sign: 1.0, weight: 0.00 }, open_mean: SigNorm { mean: 73.079, scale: 29.353, sign: -1.0, weight: 0.00 }, depth_tight: SigNorm { mean: 21.839, scale: 13.399, sign: 1.0, weight: 0.00 }, transitions: SigNorm { mean: 0.317, scale: 0.519, sign: 1.0, weight: 0.00 }, camo: SigNorm { mean: 9.237, scale: 10.235, sign: 1.0, weight: 0.00 } },
];

pub const GRANULAR_CDF: [[f64; CDF_ANCHORS]; NUM] = [
    [0.0; CDF_ANCHORS], // naked-single (trunk/unused)
    [0.0; CDF_ANCHORS], // hidden-single (trunk/unused)
    [0.0; CDF_ANCHORS], // lc-pointing (trunk/unused)
    [0.0; CDF_ANCHORS], // lc-claiming (trunk/unused)
    [0.249, 0.349, 0.399, 0.426, 0.471, 0.515, 0.566, 0.621, 1.569, 2.667, 5.639], // naked-pair
    [0.254, 0.337, 0.400, 0.432, 0.481, 0.534, 0.596, 1.524, 2.550, 3.434, 6.786], // hidden-pair
    [0.178, 0.373, 0.453, 0.529, 0.594, 0.665, 1.404, 1.557, 1.741, 2.660, 6.372], // naked-triple
    [0.177, 0.277, 0.355, 0.471, 0.503, 0.627, 1.473, 1.611, 2.502, 2.787, 5.813], // hidden-triple
    [0.185, 0.312, 0.385, 0.467, 0.546, 0.672, 1.497, 1.625, 2.424, 2.719, 4.861], // naked-quad
    [0.151, 0.315, 0.398, 0.472, 0.515, 0.590, 1.363, 1.559, 1.700, 2.628, 5.739], // hidden-quad
    [0.200, 0.299, 0.351, 0.388, 0.443, 0.507, 0.553, 0.581, 0.645, 0.718, 2.603], // x-wing
    [0.095, 0.266, 0.362, 0.403, 0.462, 0.496, 0.565, 0.619, 0.695, 0.787, 2.769], // swordfish
    [0.102, 0.273, 0.351, 0.385, 0.446, 0.515, 0.586, 0.661, 0.757, 0.881, 2.793], // jellyfish
    [0.320, 0.366, 0.396, 0.428, 0.453, 0.525, 0.574, 0.675, 1.371, 2.444, 10.778], // xy-wing
    [0.261, 0.326, 0.345, 0.383, 0.437, 0.517, 0.576, 0.737, 1.611, 2.668, 4.461], // xyz-wing
    [0.124, 0.245, 0.321, 0.408, 0.482, 0.557, 0.630, 1.427, 2.321, 3.840, 9.840], // w-wing
];

// --- Stage 4: the spec-free path's NATURAL tables (docs/grader-external-calibration.md §5) ---
//
// [`GRANULAR_NORM`]/[`GRANULAR_CDF`] above are mined over the *isolated* curriculum corpus (train/
// drill specs that force one technique), so they fit the spec-based production path ([`grade_one`]/
// [`rating`], which grades exactly those isolated-spec puzzles). A **natural** puzzle solved with the
// [`FULL_TOOLBOX`] produces a different signal distribution at the same inferred bottleneck (the
// §4.3.2 distribution shift) — so the spec-free path ([`grade_puzzle`]) reads its own NATURAL tables,
// re-mined over the gradeable dataset puzzles themselves (`grade_diag --natural-remine`). The
// production curriculum path keeps GRANULAR_* untouched, so [`grade_one`]/[`rating`] are byte-identical
// (criterion §6.5: no regression of the internal grade).
//
// A bucket the dataset cannot support a re-mine for keeps its GRANULAR_* row verbatim (the isolated
// fallback), so before any bake these are *identical* to GRANULAR_* and [`grade_puzzle`] is unchanged.
// The dense, re-mined rows are baked in below as they pass the held-out gate (§4.7); the rest alias.

/// The spec-free path's per-technique [`TechNorm`] — the curriculum [`GRANULAR_NORM`] with the Stage-4
/// dense-bucket re-mines overlaid (only the rows whose natural re-mine beat the isolated table on a
/// held-out bucket half; every other row is the isolated value, *exactly*). Read only by
/// [`grade_puzzle`]; the production [`rating`]/[`grade_one`] path keeps [`GRANULAR_NORM`].
///
/// BAKED via `datasets_correlate --natural-remine`. Accepted (2-fold held-out human-rho, §4.7):
/// **naked-pair** `normcdf` (+0.043 -> +0.067 — the NORM mean/scale re-centered on the natural
/// distribution, e.g. `alts` mean 2.18 -> 14.0); **swordfish** is `cdf`-only, so its NORM is unchanged.
pub const NATURAL_NORM: [TechNorm; NUM] = natural_norm();

const fn natural_norm() -> [TechNorm; NUM] {
    let mut n = GRANULAR_NORM;
    // naked-pair — `normcdf`: mean/scale re-centered on the natural naked-pair distribution; signs and
    // weights inherited from the Stage-3 GRANULAR_NORM row (re-mean, not re-orient/re-weight).
    n[NAKED_PAIR] = TechNorm {
        open: SigNorm { mean: 126.644, scale: 39.750, sign: -1.0, weight: 0.20 },
        cascade: SigNorm { mean: 34.027, scale: 18.729, sign: -1.0, weight: 0.05 },
        depth: SigNorm { mean: 37.877, scale: 9.004, sign: 1.0, weight: 0.00 },
        elim: SigNorm { mean: 3.048, scale: 1.402, sign: -1.0, weight: 0.35 },
        alts: SigNorm { mean: 14.027, scale: 7.140, sign: 1.0, weight: 0.30 },
        tight: SigNorm { mean: 0.122, scale: 0.086, sign: 1.0, weight: 0.10 },
        open_mean: SigNorm { mean: 127.136, scale: 38.898, sign: -1.0, weight: 0.05 },
        depth_tight: SigNorm { mean: 4.827, scale: 3.858, sign: 1.0, weight: 0.05 },
        transitions: SigNorm { mean: 0.151, scale: 0.358, sign: 1.0, weight: 0.05 },
        camo: SigNorm { mean: 37.986, scale: 20.307, sign: 1.0, weight: 0.00 },
    };
    n
}

/// The spec-free path's per-technique granular-score CDF — re-mined over the natural dataset score
/// distribution where the held-out gate accepted it, else the isolated row verbatim. The §4.3.2
/// clamping cure: natural scores that overran the isolated CDF's range pinned to a tie at `1.0`,
/// losing all within-bucket resolution. Read only by [`grade_puzzle`].
///
/// BAKED with [`NATURAL_NORM`]. Accepted (held-out, §4.7): **swordfish** `cdf` (+0.095 -> +0.166 —
/// the natural score range is [0.5, 10.7] vs the isolated CDF's [0.1, 2.8], so 63% of `armane/extreme`
/// swordfish clamped at `1.0`; the re-mined CDF restores their order); **naked-pair** `normcdf` (the
/// CDF re-derived under the re-meaned NORM above). Every other row is [`GRANULAR_CDF`] verbatim.
pub const NATURAL_CDF: [[f64; CDF_ANCHORS]; NUM] = natural_cdf();

const fn natural_cdf() -> [[f64; CDF_ANCHORS]; NUM] {
    let mut c = GRANULAR_CDF;
    c[NAKED_PAIR] = [0.211, 0.364, 0.417, 0.451, 0.526, 0.564, 0.603, 1.457, 1.584, 2.656, 5.634]; // normcdf
    c[SWORDFISH] = [0.504, 1.560, 2.436, 2.567, 3.400, 3.572, 3.703, 4.543, 4.699, 5.726, 10.741]; // cdf (un-clamp)
    c
}

/// One puzzle's **continuous within-technique rating** in `[0, 1]` — the Tier C output
/// (`docs/grader-continuous-scoring.md`): its percentile against its technique's baked
/// [`GRANULAR_CDF`], read off the [`granular_score`] under the baked [`GRANULAR_NORM`]. A
/// trunk-only spec (no harder bottleneck) rates `0.0` — uniformly easiest. The band the UI shows
/// is just [`band_of_rating`] of this (3, 5, … slices — the count is the caller's choice).
pub fn rating(spec: &Spec, s: &Signals) -> f64 {
    let Some(key) = bottleneck_key(spec) else { return 0.0 };
    let score = granular_score(s, &GRANULAR_NORM[key]);
    rating_from_cdf(score, &GRANULAR_CDF[key])
}

/// Sample mean and population standard deviation of `xs` (`(0.0, 0.0)` if empty).
fn mean_std(xs: &[f64]) -> (f64, f64) {
    let n = xs.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = xs.iter().map(|&x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    (mean, var.sqrt())
}

/// Spearman-style rank correlation of `xs` against `ys` (both aligned, same length): Pearson
/// correlation of their fractional [`percentile_ranks`]. In `[-1, 1]`; `0.0` if either is
/// constant or lengths differ. Used only to fix each signal's orientation (its sign), so the
/// magnitude is incidental — only the sign is read.
fn rank_corr(xs: &[f64], ys: &[f64]) -> f64 {
    if xs.len() != ys.len() || xs.len() < 2 {
        return 0.0;
    }
    let rx = percentile_ranks(xs);
    let ry = percentile_ranks(ys);
    let (mx, sx) = mean_std(&rx);
    let (my, sy) = mean_std(&ry);
    if sx < 1e-12 || sy < 1e-12 {
        return 0.0;
    }
    let cov = rx.iter().zip(&ry).map(|(&a, &b)| (a - mx) * (b - my)).sum::<f64>() / rx.len() as f64;
    cov / (sx * sy)
}

/// Per-technique `(gentle|medium, medium|spicy)` cut points on [`hardness_score`], indexed by
/// kind. The band is a *sub-tier* read, so each technique is cut against its **own** score
/// distribution: the pair is that kind's 33rd / 66th score percentile over a mined corpus of
/// its train+drill puzzles (see `examples/grade_diag --calibrate`), so a node splits into
/// roughly even gentle/medium/spicy thirds. Trunk kinds never key the table (a trunk-only
/// spec has no harder bottleneck → uniformly gentle), so their entries are unused placeholders.
///
/// CALIBRATED, not derived — regenerate with `examples/grade_diag --calibrate` if the
/// generator or grading solve changes. The relative [`grade_batch`] path needs no table (it
/// normalizes per batch).
///
/// NOTE (interim): [`hardness_score`] is too *quantized* for clean even-thirds cuts on some
/// techniques — `1/(1+scarcity)` only takes `{0.5, 0.33, 0.25, …}` and grind is a small
/// integer, so puzzles tie. Worst case **xyz-wing**, whose firing eliminates almost exactly
/// one candidate every time: its 33rd and 66th score percentiles coincide (`0.5, 0.5`), so the
/// medium band is empty and ~96% read spicy. The fish nodes are near-binary for the same
/// reason. These rows are the honest current cut; `docs/grader-granular-scoring.md` specifies
/// the finer signals that fix it.
pub const THRESHOLDS: [(f64, f64); NUM] = [
    (0.0, 0.0),       // naked-single  (trunk, unused)
    (0.0, 0.0),       // hidden-single (trunk, unused)
    (0.0, 0.0),       // lc-pointing   (trunk, unused)
    (0.0, 0.0),       // lc-claiming   (trunk, unused)
    (0.2000, 0.3333), // naked-pair
    (0.2000, 1.2000), // hidden-pair
    (0.2000, 1.2000), // naked-triple
    (0.1667, 1.2000), // hidden-triple
    (0.1429, 1.1667), // naked-quad
    (0.1250, 1.1250), // hidden-quad
    (0.2000, 0.2500), // x-wing
    (0.1429, 0.2000), // swordfish
    (0.1111, 0.1429), // jellyfish
    (0.3333, 0.5000), // xy-wing
    (0.5000, 0.5000), // xyz-wing  (degenerate: see note above)
    (0.5000, 1.3333), // w-wing
];

/// The technique whose [`THRESHOLDS`] row cuts a spec's puzzles: the hardest
/// (highest-[`DIFFICULTY`](crate::spec::kinds::DIFFICULTY)) *harder* bottleneck kind it
/// forces. `None` for a spec with no harder bottleneck at all (a trunk-only node) — those
/// puzzles are uniformly gentle. For a custom multi-force / `force_any` spec this is the
/// hardest member, so the band reads against the technique that dominates the puzzle.
pub fn bottleneck_key(spec: &Spec) -> Option<usize> {
    let mask = bottleneck_mask(spec);
    (NAKED_PAIR..NUM).filter(|&k| mask & (1 << k) != 0).max_by_key(|&k| DIFFICULTY[k])
}

/// The **coarse** (interim) per-puzzle 3-band cut: [`hardness_score`] against the technique's
/// [`THRESHOLDS`] row. Superseded by the granular [`band_calibrated`]; kept as the reference the
/// calibration workbench's OLD column reports against. A trunk-only spec is uniformly gentle.
pub fn band_coarse(spec: &Spec, s: &Signals) -> usize {
    let Some(key) = bottleneck_key(spec) else { return 0 };
    let (lo, hi) = THRESHOLDS[key];
    let score = hardness_score(s);
    if score < lo {
        0
    } else if score < hi {
        1
    } else {
        2
    }
}

/// Cut one puzzle's [`Signals`] to a gentle (`0`) / medium (`1`) / spicy (`2`) band — the
/// [`DEFAULT_BANDS`] (3) slicing of the continuous [`rating`], for one-puzzle-at-a-time callers.
/// The absolute, per-puzzle counterpart to [`grade_batch`]'s relative quantile cut; a trunk-only
/// spec (no harder bottleneck) is uniformly gentle. For a finer split (the UI's 5 bands) take
/// [`band_of_rating`] of [`rating`] directly.
pub fn band_calibrated(spec: &Spec, s: &Signals) -> usize {
    band_of_rating(rating(spec, s), DEFAULT_BANDS)
}

/// Grade a single puzzle **absolutely**: solve it with `spec`'s baseline toolbox via the
/// instrumented [`solve_graded`], read the [`Signals`], and map them to the continuous Tier-C
/// [`rating`] in `[0, 1]`. Returns `(signals, rating)` — the rating drives the per-puzzle
/// difficulty read (the UI cuts it into bands with [`band_of_rating`]; signals are the detailed
/// breakdown). This is the one-puzzle path (the web app, which generates one puzzle at a time);
/// [`grade_node`] / [`grade_batch`] stay the per-node *relative* path. Cold path: a single
/// easiest-first solve off the hot generation loop.
///
/// A **trunk** spec — one whose hardest forced kind is below a naked pair (the campaign's singles /
/// locked-candidate nodes, [`bottleneck_key`] `None`) — has no bottleneck for [`rating`] to score,
/// so [`rating`] returns the flat `0`. Here, where the puzzle is in hand, it is graded on its
/// fill-path frontier instead ([`trunk_rating_runs`], the production analogue of [`grade_puzzle`]'s
/// trunk branch — Stage 2 + the §4.8 averaging), giving those nodes a real within-node sub-order.
pub fn grade_one(spec: &Spec, puzzle: &DigitGrid) -> (Signals, f64) {
    let baseline = spec.baseline_mask();
    let bottleneck = bottleneck_mask(spec);
    let trace = solve_graded(&SolverState::<Bands<RowMajor>>::from_digits(puzzle), baseline);
    let signals = signals_of(&trace, bottleneck);
    let r = match bottleneck_key(spec) {
        Some(_) => rating(spec, &signals),
        None => trunk_rating_runs(puzzle, TRUNK_DEP_RUNS),
    };
    (signals, r)
}

// --- G1: spec-free grading (docs/grader-external-calibration.md §3) -------------------------
//
// Every grade above keys on a `Spec` (it tells us the forced technique and the baseline
// toolbox). An external dataset puzzle is a bare 81-char string with no spec, so it cannot use
// that path. [`grade_puzzle`] infers the bottleneck *off the solve* instead of off the spec:
// solve with the full toolbox, take the hardest harder-kind that actually fired as the
// bottleneck, and read the same within-technique [`rating`] against that kind's baked tables.

/// Every technique kind allowed at once — the toolbox [`grade_puzzle`] solves a spec-less
/// dataset puzzle with (all `NUM` bits set). Distinct from a [`Spec`]'s [`baseline_mask`](Spec::baseline_mask),
/// which scopes the toolbox to a curriculum node; a natural puzzle is unconstrained, so we let
/// every technique fire and read which one ended up the bottleneck.
pub const FULL_TOOLBOX: KindMask = (1 << NUM) - 1;

/// One spec-less puzzle's grade: the [`Signals`] read off its full-toolbox solve, the
/// **inferred** bottleneck technique, and the within-technique [`rating`]. Returned by
/// [`grade_puzzle`].
#[derive(Clone, Copy, Debug)]
pub struct PuzzleGrade {
    pub signals: Signals,
    /// The bottleneck technique inferred from the solve — the hardest ([`DIFFICULTY`])
    /// harder-than-trunk kind that fired. `None` for a puzzle the cheap closure solved on its own
    /// (no harder kind fired); such a puzzle is graded on its trunk fill-path instead ([`trunk_rating`]).
    pub key: Option<usize>,
    /// The within-technique rating in `[0, 1]`. For a keyed puzzle: its percentile against the
    /// inferred technique's baked calibration ([`GRANULAR_NORM`]/[`GRANULAR_CDF`]). For a trunk
    /// puzzle ([`key`](Self::key) `None`): the continuous trunk sub-order ([`trunk_rating`], Stage
    /// 2) off the singles + locked-candidate fill — no longer a flat `0`.
    pub rating: f64,
}

// --- Stage 2: the trunk-bucket sub-order (docs/grader-external-calibration.md §5) -----------
//
// A trunk puzzle (no harder kind fires) has no bottleneck to grade, so the per-technique [`rating`]
// is a flat `0` — and the external corpora concentrate there, pinning our within-bucket aggregate
// to ~0 on every trunk-bearing group (the §4.4 finding). Stage 2 gives the trunk a real continuous
// sub-order off the singles + locked-candidate fill ([`trunk_profile`]): the frontier-width mean
// (our deterministic analogue of Pelánek's human-validated Dependency) plus the locked-candidate
// structure singles-only Dependency lacks. Built off the trace, never tuned to Pelánek (§2.2).

/// Steps the trunk frontier is averaged over — Pelánek's Dependency window `k` (the early forced
/// chain is the discriminating part; the tail trivially widens as the grid cascades shut).
pub const TRUNK_DEP_K: usize = 25;

/// The trunk frontier mean's logistic center/scale and the locked-candidate term's weight. These
/// set only *where in `(0, 1)`* a trunk rating lands — the **within-trunk-bucket rank** the
/// calibration measures is invariant to them for the pure-singles majority (the rating is then a
/// monotone reparam of `-dependency`); they matter only for how the rarer locked-candidate puzzles
/// interleave with the singles ones. Fixed sensible defaults, NOT a calibrated fit (that is Stage
/// 3/4); the `4.5 / 2.0` pair spreads the observed trunk frontier means (~2..9) across `(0, 1)`.
const TRUNK_DEP_CENTER: f64 = 4.5;
const TRUNK_DEP_SCALE: f64 = 2.0;
const TRUNK_LC_WEIGHT: f64 = 0.9;

/// The trunk **frontier mean** (our Dependency analogue): the mean [`TrunkProfile::possibilities`]
/// over the first [`TRUNK_DEP_K`] steps. Lower = a narrower, more sequential forced chain = harder
/// (Pelánek's Dependency is the same quantity, oriented *inverse* to difficulty). `0.0` for a
/// profile with no steps (an already-complete grid — handled as trivially easy by [`trunk_rating`]).
pub fn trunk_dependency(p: &TrunkProfile) -> f64 {
    let k = TRUNK_DEP_K.min(p.possibilities.len());
    if k == 0 {
        return 0.0;
    }
    p.possibilities[..k].iter().map(|&x| x as f64).sum::<f64>() / k as f64
}

/// One trunk puzzle's continuous within-bucket **rating** in `(0, 1)` — Stage 2's replacement for
/// the flat `0`. Higher = harder: a narrower singles frontier (lower [`trunk_dependency`], oriented
/// like Pelánek's *inverse* Dependency) and more locked-candidate stalls both raise it. The
/// within-bucket rank — all `datasets_correlate` reads — is driven by `-dependency` for the
/// pure-singles majority; the locked-candidate term only lifts the puzzles that needed an LC above
/// the rest. A profile with no singles step at all (degenerate / already-complete) rates `0.0`.
pub fn trunk_rating(p: &TrunkProfile) -> f64 {
    if p.possibilities.is_empty() {
        return 0.0;
    }
    let z = (TRUNK_DEP_CENTER - trunk_dependency(p)) / TRUNK_DEP_SCALE
        + TRUNK_LC_WEIGHT * p.lc_stalls as f64;
    logistic(z)
}

// --- Stage 4: trunk-frontier averaging (docs/grader-external-calibration.md §4.5) ----------------
//
// Stage 2's [`trunk_dependency`] reads the frontier off ONE deterministic fill order; Pelánek's
// *Dependency* averages it over ~30 randomized orders, and our single read reaches only ~80-95% of
// its magnitude on the continuous trunks (§4.5 read 2). The fill order does not change *which*
// puzzle is harder, but it is a noisy estimator of the underlying mean frontier; averaging a few
// orders de-noises it toward the same quantity Pelánek measures — built off our own trace, never
// tuned to the bar (§2.2). This touches ONLY the frontier term; the locked-candidate term stays the
// deterministic Stage-2 value (Pelánek has no LC analogue, so it is out of scope for the averaging).

/// Randomized fill orders the trunk frontier is averaged over (Pelánek's Dependency runs). The
/// metric is insensitive above a handful of runs; 30 matches the Pelánek default.
pub const TRUNK_DEP_RUNS: usize = 30;

/// A stable per-puzzle RNG seed for [`trunk_profiles_rand`] — FNV-1a of the grid bytes — so the
/// averaged trunk rating is a deterministic, reproducible function of the puzzle (the same property
/// the deterministic fill had; the randomization is *within* the fill, seeded by the puzzle).
pub fn trunk_seed(puzzle: &DigitGrid) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in puzzle.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The **averaged** trunk frontier mean (Stage 4): for each step index `i`, the mean
/// [`TrunkProfile::possibilities`]`[i]` over the `profiles` runs, then the mean over the first
/// [`TRUNK_DEP_K`] steps — Pelánek's per-step-index averaging exactly. Every run places one cell per
/// step in the same total, so the per-index curve is well-defined; the `min` length guards a
/// degenerate run defensively. `0.0` for no profiles / no steps. With a single run this equals
/// [`trunk_dependency`] on that run.
pub fn trunk_dependency_avg(profiles: &[TrunkProfile]) -> f64 {
    if profiles.is_empty() {
        return 0.0;
    }
    let n_steps = profiles.iter().map(|p| p.possibilities.len()).min().unwrap_or(0);
    let k = TRUNK_DEP_K.min(n_steps);
    if k == 0 {
        return 0.0;
    }
    let mut acc = 0.0;
    for i in 0..k {
        let mean_i = profiles.iter().map(|p| p.possibilities[i] as f64).sum::<f64>()
            / profiles.len() as f64;
        acc += mean_i;
    }
    acc / k as f64
}

/// One trunk puzzle's continuous rating with the frontier term **averaged over fill orders** (Stage
/// 4) — the same logistic as [`trunk_rating`], but the dependency is [`trunk_dependency_avg`] over
/// the randomized `rand` profiles instead of the single deterministic read. The locked-candidate
/// term is read from the deterministic profile `det` (out of scope for the averaging — see the
/// module note), so an empty `rand` reproduces [`trunk_rating`] exactly.
pub fn trunk_rating_avg(det: &TrunkProfile, rand: &[TrunkProfile]) -> f64 {
    if det.possibilities.is_empty() {
        return 0.0;
    }
    let dep = if rand.is_empty() { trunk_dependency(det) } else { trunk_dependency_avg(rand) };
    let z = (TRUNK_DEP_CENTER - dep) / TRUNK_DEP_SCALE + TRUNK_LC_WEIGHT * det.lc_stalls as f64;
    logistic(z)
}

/// The spec-free trunk rating, end to end: solve the trunk fill of `puzzle` and rate it, averaging
/// the frontier over `runs` randomized fill orders (Stage 4). `runs == 0` is the deterministic
/// Stage-2 baseline ([`trunk_rating`]); `runs > 0` averages (the §4.5 refinement). The single entry
/// [`grade_puzzle`]'s trunk branch and the calibration harness both call.
pub fn trunk_rating_runs(puzzle: &DigitGrid, runs: usize) -> f64 {
    let board = SolverState::<Bands<RowMajor>>::from_digits(puzzle);
    let det = trunk_profile(&board);
    if runs == 0 {
        return trunk_rating(&det);
    }
    let rand = trunk_profiles_rand(&board, runs, trunk_seed(puzzle));
    trunk_rating_avg(&det, &rand)
}

/// Grade a **spec-less** puzzle (an external dataset entry) by inferring its bottleneck from the
/// solve rather than from a [`Spec`] (the G1 gap of `docs/grader-external-calibration.md`). Solve
/// with the [`FULL_TOOLBOX`]; the bottleneck is the hardest harder-than-trunk kind that fired, the
/// spec-less analogue of [`bottleneck_key`]'s "hardest forced member". The same featurizer and the
/// same baked per-technique tables grade it — only *which* kind is the bottleneck is read off the
/// trace.
///
/// `None` when the full-toolbox cold solve does not finish: the puzzle needs a technique above the
/// toolbox (chains), so it is **ungradeable** — that is a finding (the coverage gap), not an error.
///
/// CAVEAT (the G1 distribution shift, must be measured): the baked tables were mined from
/// *isolated* train/drill specs that force one technique and forbid easier same-branch ones. A
/// natural puzzle solved with the full toolbox produces a different signal distribution at the same
/// inferred bottleneck, so this rating is an approximation until that shift is quantified.
pub fn grade_puzzle(puzzle: &DigitGrid) -> Option<PuzzleGrade> {
    let trace = solve_graded(&SolverState::<Bands<RowMajor>>::from_digits(puzzle), FULL_TOOLBOX);
    if !trace.solved {
        return None; // needs above-toolbox logic (chains): ungradeable.
    }
    // Hardest harder-than-trunk kind that fired — read off the counts, not the spec.
    let key = (NAKED_PAIR..NUM)
        .filter(|&k| trace.counts[k] > 0)
        .max_by_key(|&k| DIFFICULTY[k]);
    match key {
        Some(k) => {
            // Spec-free path: read the NATURAL tables (Stage 4 re-mine), not the curriculum
            // GRANULAR_* — a natural puzzle's signal/score distribution differs from the isolated
            // corpus the curriculum tables were mined on (§4.3.2). Identical to GRANULAR_* for any
            // bucket without a natural re-mine, so this is byte-identical until a row is baked.
            let signals = signals_of(&trace, 1 << k);
            let rating = rating_from_cdf(granular_score(&signals, &NATURAL_NORM[k]), &NATURAL_CDF[k]);
            Some(PuzzleGrade { signals, key: Some(k), rating })
        }
        // No harder kind fired: the cheap closure (singles + locked candidates) solved it. Instead
        // of the per-technique path's flat `0`, grade the trunk on its fill-path frontier — Stage 2,
        // with the frontier averaged over [`TRUNK_DEP_RUNS`] randomized fill orders (the Stage-4
        // §4.5 refinement: the single deterministic read trails Pelánek by the averaging margin).
        None => {
            let rating = trunk_rating_runs(puzzle, TRUNK_DEP_RUNS);
            Some(PuzzleGrade { signals: signals_of(&trace, 0), key: None, rating })
        }
    }
}

// --- Stage 3: human-oriented signal signs (docs/grader-external-calibration.md §5) ----------
//
// The granular signs baked in [`GRANULAR_NORM`] are oriented by [`TechNorm::calibrate`] against the
// project's OWN relative grader (`grade_batch`) — the self-reference of §1. Stage 3 re-derives each
// LIVE signal's sign in the DENSE technique buckets from the external HUMAN label instead, leaving
// the mean/scale/weight (the §4.3.2 distribution shift is Stage 4's re-mine) and the thin buckets'
// grade_batch signs untouched. The sign is *transferred*, not literally re-fit: the human labels
// live on the dataset puzzles, the calibration on the isolated mined corpus, so the orientation is
// measured on the dataset bucket ([`human_signal_orientation`]) and applied to the corpus
// calibration ([`TechNorm::reoriented`]).

/// Minimum rankable dataset n for a technique bucket to count as **dense** — large enough to
/// re-orient its signs from the human label rather than keep the `grade_batch` fallback (§5 Stage
/// 3). Below this the per-signal human correlation is too noisy to flip a determined sign, so the
/// bucket keeps its (self-referential) `grade_batch` orientation. Picked from the Stage-1 board:
/// hidden-pair / swordfish / naked-pair / w-wing / naked-triple / xy-wing clear it; the rest do not.
pub const SIGN_DENSE_MIN: usize = 40;

/// A technique bucket's pooled per-signal **human orientation** (Stage 3). `corr[j]` is the
/// n-weighted-mean rank correlation of granular signal `j` (in [`raw_granular`] order) against the
/// human label across the dataset groups that carry this technique: its **sign** orients the signal,
/// its **magnitude** vs [`CORR_FLOOR`] says whether the human reference *determines* it. `n` is the
/// total rankable dataset puzzles behind it — the bucket is dense iff `n >= SIGN_DENSE_MIN`.
#[derive(Clone, Copy, Debug)]
pub struct HumanOrient {
    pub corr: [f64; NSIG],
    pub n: usize,
}

/// Whether every element of `xs` is equal — a constant column carries no rank information.
fn is_const(xs: &[f64]) -> bool {
    xs.first().is_none_or(|&first| xs.iter().all(|&x| x == first))
}

/// Stage 3: aggregate one technique's per-signal human orientation across dataset `groups`. Each
/// group is `(bucket_signals, human_labels)` aligned — the dataset puzzles whose inferred bottleneck
/// is this technique, with their (group-local) human difficulty label. For each granular signal the
/// per-group rank correlation against the human label is pooled n-weighted; a group contributes to a
/// signal only when both that signal column and the label vary within it (otherwise there is no
/// orientation to read). Pools the per-group CORRELATION, never the raw labels — labels are not
/// comparable across groups (the §2.3 rule). Returns the pooled `corr` per signal and the total
/// rankable `n` (the groups whose label varies, which is what "dense" is measured on).
pub fn human_signal_orientation(groups: &[(&[Signals], &[f64])]) -> HumanOrient {
    let mut num = [0.0f64; NSIG];
    let mut den = [0.0f64; NSIG];
    let mut n_total = 0usize;
    for &(sigs, labels) in groups {
        if sigs.len() < 2 || sigs.len() != labels.len() || is_const(labels) {
            continue; // no within-bucket order to test in this group
        }
        n_total += sigs.len();
        let w = sigs.len() as f64;
        let mut cols: [Vec<f64>; NSIG] = core::array::from_fn(|_| Vec::with_capacity(sigs.len()));
        for s in sigs {
            let raw = raw_granular(s);
            for (j, c) in cols.iter_mut().enumerate() {
                c.push(raw[j]);
            }
        }
        for j in 0..NSIG {
            if is_const(&cols[j]) {
                continue; // signal flat in this group -> no orientation info from it
            }
            num[j] += w * rank_corr(&cols[j], labels);
            den[j] += w;
        }
    }
    let corr = core::array::from_fn(|j| if den[j] > 0.0 { num[j] / den[j] } else { 0.0 });
    HumanOrient { corr, n: n_total }
}

impl TechNorm {
    /// Stage 3: return a copy with each **live** signal's `sign` re-set from a dense bucket's
    /// [`HumanOrient`], where the human reference determines it (`|corr| >= CORR_FLOOR`). Signals the
    /// human data leaves undetermined, and zero-weight signals, keep their existing sign. Mean, scale
    /// and weight are unchanged — this **re-orients, it does not re-weight** (the §4.3.2 distribution
    /// re-mine is Stage 4). The caller applies it only to dense buckets (`human.n >= SIGN_DENSE_MIN`).
    pub fn reoriented(&self, human: &HumanOrient) -> TechNorm {
        let orient = |sig: SigNorm, corr: f64| -> SigNorm {
            if sig.weight > 0.0 && corr.abs() >= CORR_FLOOR {
                SigNorm { sign: if corr >= 0.0 { 1.0 } else { -1.0 }, ..sig }
            } else {
                sig
            }
        };
        TechNorm {
            open: orient(self.open, human.corr[0]),
            cascade: orient(self.cascade, human.corr[1]),
            depth: orient(self.depth, human.corr[2]),
            elim: orient(self.elim, human.corr[3]),
            alts: orient(self.alts, human.corr[4]),
            tight: orient(self.tight, human.corr[5]),
            open_mean: orient(self.open_mean, human.corr[6]),
            depth_tight: orient(self.depth_tight, human.corr[7]),
            transitions: orient(self.transitions, human.corr[8]),
            camo: orient(self.camo, human.corr[9]),
        }
    }
}

/// The gentle / medium / spicy label for a band under the [`DEFAULT_BANDS`] (3) cut. For a
/// different band count the caller should print the raw `band`/`n_bands` index instead.
pub fn band_name3(band: usize) -> &'static str {
    match band {
        0 => "gentle",
        1 => "medium",
        _ => "spicy",
    }
}

/// The band count the web UI shows, cutting [`rating`] into even fifths via [`band_of_rating`].
pub const UI_BANDS: usize = 5;

/// A suggested label per band under the [`UI_BANDS`] (5) cut (`0` = gentlest). The UI owns the
/// final wording; this is a sensible default that extends the 3-band gentle/medium/spicy poles.
pub fn band_name5(band: usize) -> &'static str {
    match band {
        0 => "gentle",
        1 => "mild",
        2 => "medium",
        3 => "spicy",
        _ => "fiery",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solve::GradeStep;
    use crate::spec::kinds::{HIDDEN_SINGLE, NAKED_PAIR, X_WING, XYZ_WING};

    /// Build a trace from `(kind, paid_off, elims)` triples; `counts` is summed off the
    /// steps so `scan_work` is consistent (every step here is a harder step).
    fn trace(solved: bool, steps: &[(usize, bool, u16)]) -> GradeTrace {
        let mut counts = [0u16; NUM];
        let steps = steps
            .iter()
            .map(|&(kind, paid_off, elims)| {
                counts[kind] += 1;
                GradeStep { kind: kind as u8, paid_off, elims, ..GradeStep::default() }
            })
            .collect();
        GradeTrace { solved, counts, steps }
    }

    #[test]
    fn signals_count_dry_and_scarcity() {
        // Three X-Wing firings: first dry, second dry (back-to-back), third pays off.
        let t = trace(true, &[(X_WING, false, 2), (X_WING, false, 5), (X_WING, true, 9)]);
        let s = signals_of(&t, 1 << X_WING);
        assert_eq!(s.bottleneck_count, 3);
        assert_eq!(s.dry_firings, 2, "two firings placed nothing after");
        assert_eq!(s.longest_dry_run, 2, "the first two are back-to-back dry");
        assert_eq!(s.scarcity, 2, "tightest (fewest-elim) firing defines scarcity");
        assert_eq!(s.scan_work, 3, "three harder steps");
    }

    #[test]
    fn dry_run_breaks_on_payoff() {
        // dry, pay, dry, dry, dry(solve) -> longest run is the trailing 3.
        let t = trace(
            true,
            &[(X_WING, false, 1), (X_WING, true, 1), (X_WING, false, 1), (X_WING, false, 1), (X_WING, false, 1)],
        );
        let s = signals_of(&t, 1 << X_WING);
        assert_eq!(s.longest_dry_run, 3);
        assert_eq!(s.dry_firings, 4);
    }

    #[test]
    fn no_bottleneck_is_max_scarcity() {
        // A puzzle that needs no harder step at all (all cheap): empty step list.
        let t = trace(true, &[]);
        let s = signals_of(&t, 1 << X_WING);
        assert_eq!(s.bottleneck_count, 0);
        assert_eq!(s.scarcity, u32::MAX);
        assert_eq!(s.scan_work, 0);
    }

    #[test]
    fn percentile_ranks_handle_ties() {
        let r = percentile_ranks(&[1.0, 1.0, 3.0, 2.0]);
        // The two 1.0s share the low mid-rank; 3.0 is the top.
        assert_eq!(r[0], r[1]);
        assert!(r[0] < r[3] && r[3] < r[2]);
    }

    #[test]
    fn banding_is_monotone_in_difficulty() {
        // Five puzzles, strictly increasing dry runs (the dominant signal): bands must be
        // non-decreasing in that order, gentlest first and spiciest last.
        let bn = 1 << X_WING;
        let traces: Vec<GradeTrace> = (0..6)
            .map(|k| {
                let steps: Vec<(usize, bool, u16)> =
                    (0..=k).map(|_| (X_WING, false, 3)).chain([(X_WING, true, 3)]).collect();
                trace(true, &steps)
            })
            .collect();
        let g = grade_batch(&traces, bn, &Weights::default(), 3);
        assert_eq!(g.len(), 6);
        for w in g.windows(2) {
            assert!(w[0].band <= w[1].band, "harder puzzle must not land in a gentler band");
        }
        assert_eq!(g[0].band, 0, "easiest in the gentlest band");
        assert_eq!(g[5].band, 2, "hardest in the spiciest band");
    }

    #[test]
    fn empty_batch_grades_to_nothing() {
        assert!(grade_batch(&[], 1 << NAKED_PAIR, &Weights::default(), 3).is_empty());
    }

    /// Build [`Signals`] from the fields the absolute [`hardness_score`] reads.
    fn sig(bottleneck_count: u32, longest_dry_run: u32, scarcity: u32) -> Signals {
        Signals { bottleneck_count, longest_dry_run, scarcity, ..Signals::default() }
    }

    #[test]
    fn hardness_score_zero_without_bottleneck() {
        // No bottleneck firing -> uniformly gentle, regardless of an incidental dry run over
        // some other harder kind.
        assert_eq!(hardness_score(&sig(0, 5, u32::MAX)), 0.0);
    }

    #[test]
    fn hardness_score_orders_grind_then_scarcity() {
        let a = hardness_score(&sig(1, 0, 9)); // grind 0, plentiful
        let b = hardness_score(&sig(1, 0, 1)); // grind 0, scarce -> higher
        let c = hardness_score(&sig(2, 0, 9)); // grind 1 -> above any grind-0
        assert!(a < b, "scarcer firing scores higher within a grind level");
        assert!(b < c, "one more firing outranks any scarcity tiebreak");
        // Scarcity only ever sub-orders: it never bumps a puzzle a whole grind level.
        assert!(b < 1.0 && c >= 1.0);
    }

    #[test]
    fn band_calibrated_trunk_is_gentle() {
        // A trunk-only spec has no harder bottleneck -> rating 0 -> gentlest band.
        let spec = Spec::explicit().force(HIDDEN_SINGLE, 1);
        assert_eq!(bottleneck_key(&spec), None);
        assert_eq!(rating(&spec, &sig(1, 0, 1)), 0.0);
        assert_eq!(band_calibrated(&spec, &sig(1, 0, 1)), 0);
    }

    #[test]
    fn grade_one_trunk_spec_gets_continuous_rating() {
        // The production path: a trunk spec (forces only a locked candidate — no kind >= naked pair,
        // so bottleneck_key is None) used to rate flat 0 via `rating`. grade_one now grades the
        // campaign's singles/LC node on its fill-path frontier -> a continuous (0,1) within-node
        // rating, while the signals-only `rating`/`band_calibrated` (no puzzle) stay flat 0.
        use crate::spec::kinds::LC_POINTING;
        let spec = Spec::train_isolated(LC_POINTING);
        assert_eq!(bottleneck_key(&spec), None, "a locked-candidate spec is a trunk spec");
        let classic = "53..7....6..195....98....6.8...6...34..8.3..17...2...6.6....28....419..5....8..79";
        let grid = DigitGrid::parse(classic).unwrap();
        let (_signals, r) = grade_one(&spec, &grid);
        assert!(r > 0.0 && r < 1.0, "trunk spec now rates continuously in (0,1), not flat 0");
        // The signals-only path is unchanged (it has no puzzle to profile).
        assert_eq!(rating(&spec, &_signals), 0.0, "rating(spec, signals) stays flat 0 for a trunk spec");
    }

    #[test]
    fn band_coarse_cuts_by_technique_threshold() {
        // The (kept) coarse path: x-wing THRESHOLDS row (0.20, 0.25), pressure = 1/(1+scarcity).
        let spec = Spec::train_isolated(X_WING);
        assert_eq!(bottleneck_key(&spec), Some(X_WING));
        assert_eq!(band_coarse(&spec, &sig(1, 0, 9)), 0, "pressure 0.10 < 0.20 -> gentle");
        assert_eq!(band_coarse(&spec, &sig(1, 0, 4)), 1, "pressure 0.20 in [0.20,0.25) -> medium");
        assert_eq!(band_coarse(&spec, &sig(2, 0, 9)), 2, "grind 1 -> score >= 1.0 -> spicy");
    }

    // --- Tier C: continuous rating -----------------------------------------------------

    #[test]
    fn rating_from_cdf_interpolates_and_clamps() {
        // A linear CDF 0..10: a score maps to its own /10 percentile, clamped at the ends.
        let cdf: [f64; CDF_ANCHORS] = core::array::from_fn(|i| i as f64);
        assert_eq!(rating_from_cdf(-1.0, &cdf), 0.0, "below min clamps to 0");
        assert_eq!(rating_from_cdf(11.0, &cdf), 1.0, "above max clamps to 1");
        assert!((rating_from_cdf(5.0, &cdf) - 0.5).abs() < 1e-9, "midpoint -> 0.5");
        assert!(rating_from_cdf(2.5, &cdf) < rating_from_cdf(7.5, &cdf), "monotone in score");
    }

    #[test]
    fn band_of_rating_partitions_into_n() {
        assert_eq!(band_of_rating(0.0, 5), 0);
        assert_eq!(band_of_rating(0.19, 5), 0);
        assert_eq!(band_of_rating(0.20, 5), 1);
        assert_eq!(band_of_rating(0.99, 5), 4);
        assert_eq!(band_of_rating(1.0, 5), 4, "rating 1.0 stays in the top band");
    }

    /// Full [`Signals`] for an X-Wing stall (the granular fields the rating reads).
    fn xwing_sig(open: u32, elim: u32, alts: u32) -> Signals {
        Signals {
            bottleneck_count: 1,
            longest_dry_run: 0,
            scarcity: elim,
            open,
            elim_sum: elim,
            alts,
            ..Signals::default()
        }
    }

    #[test]
    fn rating_is_bounded_and_orders_by_scarcity() {
        // Against the baked x-wing calibration, the rating is in [0,1] and a scarcer stall
        // (fewer eliminations — the dominant fish signal, oriented lower = harder) rates higher.
        let spec = Spec::train_isolated(X_WING);
        let plentiful = rating(&spec, &xwing_sig(110, 8, 1));
        let scarce = rating(&spec, &xwing_sig(110, 1, 1));
        assert!((0.0..=1.0).contains(&plentiful) && (0.0..=1.0).contains(&scarce));
        assert!(scarce >= plentiful, "fewer eliminations = harder = higher rating");
        // The 5-band UI cut of the rating stays in range.
        assert!(band_of_rating(scarce, UI_BANDS) < UI_BANDS);
    }

    #[test]
    fn xyz_wing_rating_rises_with_camo() {
        // Tier E: camo (E1 near-miss camouflage) is weighted only for xyz-wing (the §2.3 residual).
        // Against the baked xyz-wing calibration, two stalls equal in every deduction signal but
        // differing in camo must order by it — more dead look-alikes = harder to spot = higher.
        let spec = Spec::drill_isolated(XYZ_WING);
        assert_eq!(bottleneck_key(&spec), Some(XYZ_WING));
        let stall = |camo| Signals {
            bottleneck_count: 1,
            longest_dry_run: 0,
            scarcity: 1,
            open: 82,
            cascade: 25,
            elim_sum: 1,
            alts: 1,
            camo,
            ..Signals::default()
        };
        let few = rating(&spec, &stall(2));
        let many = rating(&spec, &stall(14));
        assert!((0.0..=1.0).contains(&few) && (0.0..=1.0).contains(&many));
        assert!(many > few, "more near-miss camouflage = harder = higher xyz-wing rating");
    }

    // --- granular score (Tier A) -------------------------------------------------------

    /// Build a trace from `(kind, paid_off, elims, open, depth, cascade, alts, examined)`
    /// 8-tuples — the full per-step record the granular signals reduce from.
    fn trace_full(steps: &[(usize, bool, u16, u32, u16, u16, u32, u32)]) -> GradeTrace {
        let mut counts = [0u16; NUM];
        let steps = steps
            .iter()
            .map(|&(kind, paid_off, elims, open, depth, cascade, alts, examined)| {
                counts[kind] += 1;
                GradeStep { kind: kind as u8, paid_off, elims, open, depth, cascade, alts, examined }
            })
            .collect();
        GradeTrace { solved: true, counts, steps }
    }

    #[test]
    fn signals_of_reads_tightest_open_and_first_depth() {
        // Two X-Wing firings: first looser (5 elims, open 120, depth 30, alts 7, examined 9),
        // second tighter (2 elims, open 90, depth 40, alts 2, examined 6). `open`/`cascade`/`alts`
        // and `camo` come from the TIGHTEST (fewest-elim) firing; `depth` from the FIRST.
        let t =
            trace_full(&[(X_WING, true, 5, 120, 30, 4, 7, 9), (X_WING, true, 2, 90, 40, 1, 2, 6)]);
        let s = signals_of(&t, 1 << X_WING);
        assert_eq!(s.scarcity, 2, "tightest = fewest elims");
        assert_eq!(s.open, 90, "open read at the tightest firing");
        assert_eq!(s.cascade, 1, "cascade read at the tightest firing");
        assert_eq!(s.alts, 2, "alts read at the tightest firing");
        assert_eq!(s.camo, 4, "camo = examined - alts at the tightest firing (6 - 2)");
        assert_eq!(s.depth, 30, "depth read at the first firing");
        assert_eq!(s.elim_sum, 7, "elim_sum sums all firings");
    }

    /// A [`TechNorm`] driven by a single live signal (`open`, higher = harder), the others off.
    fn open_only_norm() -> TechNorm {
        let off = SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 };
        TechNorm {
            open: SigNorm { mean: 100.0, scale: 10.0, sign: 1.0, weight: 1.0 },
            cascade: off,
            depth: off,
            elim: off,
            alts: off,
            tight: off,
            open_mean: off,
            depth_tight: off,
            transitions: off,
            camo: off,
        }
    }

    fn sig_open(bottleneck_count: u32, longest_dry_run: u32, open: u32) -> Signals {
        Signals { bottleneck_count, longest_dry_run, open, ..Signals::default() }
    }

    #[test]
    fn granular_score_zero_without_bottleneck() {
        assert_eq!(granular_score(&sig_open(0, 5, 150), &open_only_norm()), 0.0);
    }

    #[test]
    fn granular_blend_stays_within_grind_level() {
        // The continuous blend is strictly in (0, 1), so grind is never crossed: a grind-0
        // puzzle (however busy) stays below any grind-1 puzzle (however sparse).
        let busy0 = granular_score(&sig_open(1, 0, 200), &open_only_norm()); // grind 0
        let sparse1 = granular_score(&sig_open(2, 0, 10), &open_only_norm()); // grind 1
        assert!(busy0 < 1.0, "grind-0 score stays below 1");
        assert!(sparse1 >= 1.0, "grind-1 score is at least 1");
        assert!(busy0 < sparse1, "one more firing outranks any continuous sub-order");
    }

    #[test]
    fn granular_score_monotone_in_open() {
        // Within a grind level, a higher `open` (sign +1) scores strictly higher.
        let lo = granular_score(&sig_open(1, 0, 80), &open_only_norm());
        let hi = granular_score(&sig_open(1, 0, 120), &open_only_norm());
        assert!(lo < hi, "higher open => harder under sign +1");
    }

    // --- G1: spec-free grading ---------------------------------------------------------

    #[test]
    fn grade_puzzle_none_when_unsolvable_by_toolbox() {
        // An empty grid: no single or harder technique can fire (every cell holds all 9
        // candidates), so the full-toolbox solve stalls at once and never finishes -> ungradeable.
        assert!(grade_puzzle(&DigitGrid::EMPTY).is_none());
    }

    #[test]
    fn grade_puzzle_infers_bottleneck_and_rates_in_range() {
        // A puzzle the generator built to need a naked pair: grade it spec-free. The full-toolbox
        // solve must finish, infer some harder bottleneck, and rate within [0, 1].
        use crate::generate::{AttemptResult, attempt};
        use crate::rng::Rng;
        let spec = Spec::train(NAKED_PAIR);
        let mut graded = false;
        for seed in 0..2000u64 {
            if let AttemptResult::Success(p) = attempt(&mut Rng::from_seed(seed), &spec) {
                let g = grade_puzzle(&p.puzzle.0).expect("a baseline-solvable puzzle grades");
                assert!(g.key.is_some(), "a naked-pair puzzle needs a harder kind");
                assert!((0.0..=1.0).contains(&g.rating));
                graded = true;
                break;
            }
        }
        assert!(graded, "the generator should yield a naked-pair puzzle within the seed budget");
    }

    // --- Stage 2: trunk-bucket sub-order ------------------------------------------------

    /// A trunk profile with `n` steps all of frontier `f`, and `lc_stalls` locked-candidate stalls.
    fn trunk(f: u32, n: usize, lc_stalls: u32) -> TrunkProfile {
        TrunkProfile { possibilities: vec![f; n], lc_elims: lc_stalls, lc_stalls, solved: true }
    }

    #[test]
    fn trunk_rating_is_bounded_and_orders_by_frontier() {
        // A narrower singles frontier (fewer simultaneous forced cells) is a more sequential,
        // harder chain -> a higher trunk rating (our inverse-Dependency orientation).
        let wide = trunk_rating(&trunk(9, 30, 0));
        let narrow = trunk_rating(&trunk(2, 30, 0));
        assert!((0.0..=1.0).contains(&wide) && (0.0..=1.0).contains(&narrow));
        assert!(narrow > wide, "a narrower frontier = harder = higher rating");
    }

    #[test]
    fn trunk_rating_rises_with_lc_stalls() {
        // Two puzzles with the same frontier but one needing a locked-candidate stall: the LC one
        // is strictly harder (a cheap technique above singles), so it rates higher.
        let pure = trunk_rating(&trunk(5, 30, 0));
        let with_lc = trunk_rating(&trunk(5, 30, 1));
        assert!(with_lc > pure, "needing a locked candidate = harder = higher rating");
    }

    #[test]
    fn trunk_rating_zero_without_steps() {
        // A degenerate profile with no singles step (already-complete / no chain) is trivially easy.
        assert_eq!(trunk_rating(&TrunkProfile::default()), 0.0);
    }

    // --- Stage 4: trunk-frontier averaging (§4.5) ---------------------------------------

    #[test]
    fn trunk_dependency_avg_means_per_step_index() {
        // Per step index, average the frontier across runs; then mean over the first k steps. Two
        // runs (3,3,..) and (5,5,..) average to (4,4,..) -> dependency 4.0.
        let a = trunk(3, 30, 0);
        let b = trunk(5, 30, 0);
        assert!((trunk_dependency_avg(&[a, b]) - 4.0).abs() < 1e-9);
        // A single run reproduces the deterministic dependency exactly.
        let one = trunk(7, 30, 0);
        assert!((trunk_dependency_avg(std::slice::from_ref(&one)) - trunk_dependency(&one)).abs() < 1e-9);
        // No profiles / no steps -> 0.0 (degenerate, trivially easy).
        assert_eq!(trunk_dependency_avg(&[]), 0.0);
    }

    #[test]
    fn trunk_rating_avg_empty_runs_equals_deterministic() {
        // With no randomized runs the averaged rating falls back to the deterministic Stage-2 rating
        // (the frontier term reads `det`; the LC term always does).
        let det = trunk(4, 30, 1);
        assert_eq!(trunk_rating_avg(&det, &[]), trunk_rating(&det));
    }

    #[test]
    fn trunk_rating_avg_uses_det_lc_and_averaged_frontier() {
        // The LC term comes from `det` (out of scope for the averaging); only the frontier is
        // averaged. det frontier 9 (wide/easy) but rand frontier 2 (narrow/hard) -> the averaged
        // rating reads the rand frontier, so it is strictly harder than rating(det) alone.
        let det = trunk(9, 30, 0);
        let rand = vec![trunk(2, 30, 0), trunk(2, 30, 0)];
        let avg = trunk_rating_avg(&det, &rand);
        assert!(avg > trunk_rating(&det), "the averaged narrow frontier rates harder than det's wide one");
        assert!((0.0..=1.0).contains(&avg));
    }

    #[test]
    fn trunk_rating_runs_is_reproducible_and_finishes() {
        // The averaged spec-free trunk rating is a deterministic function of the puzzle (seeded from
        // its bytes), in (0,1), and stable across calls.
        let classic = "53..7....6..195....98....6.8...6...34..8.3..17...2...6.6....28....419..5....8..79";
        let grid = DigitGrid::parse(classic).unwrap();
        let r1 = trunk_rating_runs(&grid, TRUNK_DEP_RUNS);
        let r2 = trunk_rating_runs(&grid, TRUNK_DEP_RUNS);
        assert_eq!(r1, r2, "seeded from the puzzle -> reproducible");
        assert!(r1 > 0.0 && r1 < 1.0);
    }

    #[test]
    fn grade_puzzle_trunk_gets_continuous_rating() {
        // The classic singles-only puzzle is solved by the cheap closure alone (no harder kind
        // fires), so it keys to the trunk bucket — and Stage 2 grades it on its fill-path frontier
        // to a continuous rating rather than the old flat `0`.
        let classic = "53..7....6..195....98....6.8...6...34..8.3..17...2...6.6....28....419..5....8..79";
        let g = grade_puzzle(&DigitGrid::parse(classic).unwrap()).expect("singles puzzle solves");
        assert_eq!(g.key, None, "the classic puzzle is singles-only (trunk bucket)");
        assert!(g.rating > 0.0 && g.rating < 1.0, "trunk rating is a continuous (0,1), not flat 0");
    }

    #[test]
    fn calibrate_orients_signs_and_drops_constant_signals() {
        // Four puzzles, relative hardness ascending. `open`/`depth` rise with it (-> sign +1),
        // `elim` falls with it (scarcer = harder -> sign -1), `cascade` is constant (-> dropped).
        let mk = |open, elim, cascade, depth| Signals {
            bottleneck_count: 1,
            open,
            elim_sum: elim,
            cascade,
            depth,
            ..Signals::default()
        };
        let sample =
            [mk(10, 40, 5, 10), mk(20, 30, 5, 20), mk(30, 20, 5, 30), mk(40, 10, 5, 40)];
        let rel = [0.0, 1.0, 2.0, 3.0];
        let norm = TechNorm::calibrate(NAKED_PAIR, &sample, &rel);
        assert_eq!(norm.open.sign, 1.0, "open rises with hardness");
        assert_eq!(norm.elim.sign, -1.0, "scarcer (fewer elims) is harder");
        assert_eq!(norm.depth.sign, 1.0);
        // Subset branch weights: open 0.30, depth 0.10, elim 0.50 (live); cascade dropped.
        assert!(norm.open.weight > 0.0 && norm.elim.weight > 0.0 && norm.depth.weight > 0.0);
        assert_eq!(norm.cascade.weight, 0.0, "a constant signal carries no weight");
    }

    // --- Stage 3: human-oriented signs --------------------------------------------------

    #[test]
    fn human_orientation_pools_per_group_and_skips_flat_labels() {
        // Two groups for one technique. In both, `open` rises with the human label (-> +corr) and
        // `elim` falls with it (-> -corr). A third group whose label is constant must contribute
        // nothing (no within-bucket order). `n` counts only the rankable groups' puzzles.
        let mk = |open, elim| Signals { bottleneck_count: 1, open, elim_sum: elim, ..Signals::default() };
        let g1: Vec<Signals> = vec![mk(10, 40), mk(20, 30), mk(30, 20), mk(40, 10)];
        let l1 = vec![1.0, 2.0, 3.0, 4.0];
        let g2: Vec<Signals> = vec![mk(15, 9), mk(25, 6), mk(35, 3)];
        let l2 = vec![10.0, 20.0, 30.0];
        let flat: Vec<Signals> = vec![mk(99, 1), mk(50, 2)];
        let lflat = vec![5.0, 5.0]; // constant label -> excluded
        let groups: Vec<(&[Signals], &[f64])> =
            vec![(g1.as_slice(), &l1), (g2.as_slice(), &l2), (flat.as_slice(), &lflat)];
        let h = human_signal_orientation(&groups);
        assert_eq!(h.n, 7, "only the two rankable groups (4 + 3) count toward n");
        assert!(h.corr[0] > CORR_FLOOR, "open rises with the human label across both groups");
        assert!(h.corr[3] < -CORR_FLOOR, "elim falls with the human label (scarcer = harder)");
    }

    #[test]
    fn reoriented_flips_live_signs_only_when_determined() {
        // A norm with `alts` baked sign +1 (live, weight 0.45) and `cascade` sign -1 (live). Human
        // says alts should be -1 (strong negative corr) and cascade is undetermined (|corr| < floor):
        // alts flips, cascade and the (zero-weight) others stay.
        let live = |sign| SigNorm { mean: 0.0, scale: 1.0, sign, weight: 0.45 };
        let off = SigNorm { mean: 0.0, scale: 1.0, sign: 1.0, weight: 0.0 };
        let norm = TechNorm {
            open: off,
            cascade: SigNorm { weight: 0.15, ..live(-1.0) },
            depth: off,
            elim: off,
            alts: live(1.0),
            tight: off,
            open_mean: off,
            depth_tight: off,
            transitions: off,
            camo: off,
        };
        let mut corr = [0.0f64; NSIG];
        corr[4] = -0.6; // alts: strong negative -> flip to -1
        corr[1] = 0.05; // cascade: below the floor -> keep
        corr[0] = -0.9; // open: zero-weight -> ignored
        let r = norm.reoriented(&HumanOrient { corr, n: 100 });
        assert_eq!(r.alts.sign, -1.0, "a determined human sign flips the live signal");
        assert_eq!(r.cascade.sign, -1.0, "an undetermined signal keeps its grade_batch sign");
        assert_eq!(r.open.sign, 1.0, "a zero-weight signal is never re-oriented");
        assert_eq!(r.alts.weight, 0.45, "re-orient does not re-weight");
    }

    // --- Stage 4: natural-puzzle re-mine ------------------------------------------------

    #[test]
    fn remeaned_recenters_mean_scale_keeps_sign_and_weight() {
        // One live signal (`open`, sign -1, weight 0.5) plus a constant-in-sample one (`cascade`).
        let live = SigNorm { mean: 100.0, scale: 10.0, sign: -1.0, weight: 0.5 };
        let off = SigNorm { mean: 5.0, scale: 1.0, sign: 1.0, weight: 0.0 };
        let base = TechNorm {
            open: live, cascade: off, depth: off, elim: off, alts: off,
            tight: off, open_mean: off, depth_tight: off, transitions: off, camo: off,
        };
        // `open` takes 10/20/30 (mean 20, pop-std ~8.165); every other raw signal is constant 0.
        let mk = |open| Signals { bottleneck_count: 1, open, ..Signals::default() };
        let r = base.remeaned(&[mk(10), mk(20), mk(30)]);
        assert!((r.open.mean - 20.0).abs() < 1e-9, "mean re-centered on the natural sample");
        assert!((r.open.scale - 8.16497).abs() < 1e-3, "scale = pop std of the natural sample");
        assert_eq!(r.open.sign, -1.0, "re-mean inherits the sign (does not re-orient)");
        assert_eq!(r.open.weight, 0.5, "re-mean inherits the weight (does not re-weight)");
        assert_eq!(r.cascade, off, "a signal constant across the sample keeps its baked params");
    }

    #[test]
    fn natural_tables_overlay_only_the_accepted_buckets() {
        // Production-safety guard: the spec-free NATURAL_* tables must equal the curriculum
        // GRANULAR_* everywhere EXCEPT the two held-out-accepted Stage-4 buckets (naked-pair normcdf,
        // swordfish cdf-only). Any other drift would silently change grading on an unvalidated bucket.
        for k in 0..NUM {
            if k == NAKED_PAIR {
                assert_ne!(NATURAL_CDF[k], GRANULAR_CDF[k], "naked-pair CDF is re-mined");
                assert_ne!(NATURAL_NORM[k], GRANULAR_NORM[k], "naked-pair NORM is re-meaned (normcdf)");
            } else if k == SWORDFISH {
                assert_ne!(NATURAL_CDF[k], GRANULAR_CDF[k], "swordfish CDF is re-mined (un-clamp)");
                assert_eq!(NATURAL_NORM[k], GRANULAR_NORM[k], "swordfish is cdf-only: NORM unchanged");
            } else {
                assert_eq!(NATURAL_NORM[k], GRANULAR_NORM[k], "non-accepted NORM stays the isolated row");
                assert_eq!(NATURAL_CDF[k], GRANULAR_CDF[k], "non-accepted CDF stays the isolated row");
            }
        }
    }

    #[test]
    fn natural_swordfish_cdf_unclamps_the_isolated_range() {
        // The §4.3.2 distribution shift, quantified: natural swordfish scores (up to ~10.7) overran
        // the isolated CDF's top anchor (~2.77), pinning 63% of armane/extreme at rating 1.0. The
        // re-mined CDF's top anchor must reach the natural range, so those scores no longer clamp.
        let nat = NATURAL_CDF[SWORDFISH][CDF_ANCHORS - 1];
        let iso = GRANULAR_CDF[SWORDFISH][CDF_ANCHORS - 1];
        assert!(nat > 2.0 * iso, "natural swordfish CDF spans far past the isolated max ({nat} vs {iso})");
        // The CDF is monotone non-decreasing (cdf_of guarantees it) — a sanity check on the bake.
        for w in NATURAL_CDF[SWORDFISH].windows(2) {
            assert!(w[1] >= w[0], "natural swordfish CDF must be non-decreasing");
        }
    }
}
