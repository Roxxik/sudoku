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
use crate::solve::{GradeTrace, solve_graded};
use crate::spec::Spec;
use crate::spec::kinds::{DIFFICULTY, KindMask, NAKED_PAIR, NUM};

/// The default band count — gentle / medium / spicy, the plan's natural per-node cut.
pub const DEFAULT_BANDS: usize = 3;

/// The four sub-tier difficulty signals read off one easiest-first solve. All are oriented
/// in the *raw* direction the trace produces them; [`grade_batch`] applies the
/// harder-direction orientation when it ranks (only [`scarcity`](Self::scarcity) is "lower
/// is harder").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    for step in &trace.steps {
        let is_bottleneck = bottleneck & (1 << step.kind) != 0;
        if is_bottleneck {
            s.bottleneck_count += 1;
            s.scarcity = s.scarcity.min(step.elims as u32);
        }
        if step.paid_off {
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
    let n_bands = n_bands.max(1);
    let signals: Vec<Signals> = traces.iter().map(|t| signals_of(t, bottleneck)).collect();
    let n = signals.len();
    if n == 0 {
        return Vec::new();
    }

    // Per-signal hardness ranks. The two dry components are averaged into one "dry" rank
    // so signal 2 stays a single weighted term; scarcity is inverted (lower = harder).
    let count_r = percentile_ranks(&col(&signals, |s| s.bottleneck_count as f64));
    let dryf_r = percentile_ranks(&col(&signals, |s| s.dry_firings as f64));
    let dryr_r = percentile_ranks(&col(&signals, |s| s.longest_dry_run as f64));
    let scar_r = percentile_ranks(&col(&signals, |s| s.scarcity as f64));
    let scan_r = percentile_ranks(&col(&signals, |s| s.scan_work as f64));

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

/// Cut one puzzle's [`Signals`] to a gentle (`0`) / medium (`1`) / spicy (`2`) band against
/// its technique's [`THRESHOLDS`] row — the absolute, per-puzzle counterpart to
/// [`grade_batch`]'s relative quantile cut, for one-puzzle-at-a-time callers. A trunk-only
/// spec (no harder bottleneck) is uniformly gentle.
pub fn band_calibrated(spec: &Spec, s: &Signals) -> usize {
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

/// Grade a single puzzle **absolutely**: solve it with `spec`'s baseline toolbox via the
/// instrumented [`solve_graded`], read the four [`Signals`], and cut them to a stable band
/// with [`band_calibrated`]. Returns `(signals, band)` — the band drives a per-puzzle
/// difficulty label and the signals a detailed readout. This is the one-puzzle path (the web
/// app, which generates one puzzle at a time); [`grade_node`] / [`grade_batch`] stay the
/// per-node *relative* path for grading a whole batch against itself. Cold path: a single
/// easiest-first solve off the hot generation loop.
pub fn grade_one(spec: &Spec, puzzle: &DigitGrid) -> (Signals, usize) {
    let baseline = spec.baseline_mask();
    let bottleneck = bottleneck_mask(spec);
    let trace = solve_graded(&SolverState::<Bands<RowMajor>>::from_digits(puzzle), baseline);
    let signals = signals_of(&trace, bottleneck);
    (signals, band_calibrated(spec, &signals))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solve::GradeStep;
    use crate::spec::kinds::{HIDDEN_SINGLE, NAKED_PAIR, X_WING};

    /// Build a trace from `(kind, paid_off, elims)` triples; `counts` is summed off the
    /// steps so `scan_work` is consistent (every step here is a harder step).
    fn trace(solved: bool, steps: &[(usize, bool, u16)]) -> GradeTrace {
        let mut counts = [0u16; NUM];
        let steps = steps
            .iter()
            .map(|&(kind, paid_off, elims)| {
                counts[kind] += 1;
                GradeStep { kind: kind as u8, paid_off, elims }
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
        // A trunk-only spec has no harder bottleneck -> no threshold row -> gentle.
        let spec = Spec::explicit().force(HIDDEN_SINGLE, 1);
        assert_eq!(bottleneck_key(&spec), None);
        assert_eq!(band_calibrated(&spec, &sig(1, 0, 1)), 0);
    }

    #[test]
    fn band_calibrated_cuts_by_technique_threshold() {
        // The x-wing row is (0.20, 0.25); pressure = 1/(1+scarcity).
        let spec = Spec::train_isolated(X_WING);
        assert_eq!(bottleneck_key(&spec), Some(X_WING));
        assert_eq!(band_calibrated(&spec, &sig(1, 0, 9)), 0, "pressure 0.10 < 0.20 -> gentle");
        assert_eq!(band_calibrated(&spec, &sig(1, 0, 4)), 1, "pressure 0.20 in [0.20,0.25) -> medium");
        assert_eq!(band_calibrated(&spec, &sig(2, 0, 9)), 2, "grind 1 -> score >= 1.0 -> spicy");
    }
}
