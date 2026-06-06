//! The spec-driven `random`-method generator on the new `repr` foundations — the
//! layered prober/solver stack ([`probe`](crate::probe) for uniqueness, [`solve`](crate::solve) for the
//! difficulty gate).
//!
//! Per attempt: a random full grid ([`random_solution`]), then strip cells in random
//! order keeping a strip iff the puzzle stays unique (the [`Search`] prober) AND
//! baseline-solvable (the spec toolbox, via [`FusedLogicSolver`]). The most-stripped
//! state whose baseline trace meets the requirement counts is remembered as `best`;
//! after the strip, if a `best` exists and it passes [`verify`], the attempt succeeds.
//! This is the exact gate sequence — `alts == 0` fast path, uniqueness gate, baseline
//! gate, `req_met`/`best`/verify — of the old bb-based `generator` strip attempt it
//! replaced, so for a given seed it produces byte-identical puzzles (the warp still
//! runs that bb strip; `tests/equiv_warp` pins the two lane-for-lane).
//!
//! The candidate state is carried incrementally across the 81 strip steps exactly as
//! bb does: a single [`DualBandedMarkGrid`] plus a `clue` map, reopened/reclosed in
//! place with [`clear_clue`](DualBandedMarkGrid::clear_clue) /
//! [`place_clue`](DualBandedMarkGrid::place_clue) rather than rebuilt each step. Holding
//! both bandings live means the uniqueness prober reads the row view and the baseline
//! gate reads both — with **no per-gate reconstruction**, just as bb reuses one dual
//! `BitBoard` for both gates. The lone `from_digits` is once per attempt, on the full
//! solution.

use crate::fill::random_solution;
use crate::probe::{Prober, Search};
use crate::repr::banded::{Bands, DualBandedMarkGrid, RowMajor};
use crate::repr::{Board, CELLS, Digit, DigitGrid, Mark, Marks, PerDigit, Puzzle, Solution};
use crate::rng::Rng;
use crate::scan::Bivalue;
use crate::solve::{FusedLogicSolver, LogicSolver, Solver};
use crate::spec::Spec;
use crate::technique_kinds::{
    HIDDEN_SINGLE, KindMask, LC_CLAIMING, LC_POINTING, NAKED_SINGLE,
};
use crate::util::{FNV_OFFSET, FNV_PRIME, fnv_fold_cells};

/// The banded packing the prober branches on and the incremental strip is carried in.
type RM = Bands<RowMajor>;
/// The uniqueness prober: the scan/sieve [`Search`] with the [`Bivalue`] branch
/// strategy — the optimal prober strategy for the strip (see the prober memos).
type P = Search<Bivalue>;

/// A generated puzzle and the full solution it was stripped from.
pub struct GeneratedPuzzle {
    pub puzzle: Puzzle,
    pub solution: Solution,
    pub givens: usize,
}

/// Why a single attempt ended — the new-repr twin of the old bb `generator`'s `AttemptResult`.
pub enum AttemptResult {
    /// A puzzle satisfying the spec (passed verify).
    Success(GeneratedPuzzle),
    /// A requirement-meeting `best` was found but verify rejected it (the target was
    /// substitutable). Core's `requirement_not_forced`.
    NotForced,
    /// No strip ever met the requirement counts. Core's `requirement_never_fired`.
    NeverFired,
}

/// The cells of `digits` as a bare `[u8; 81]` (`0` = empty) — the form the
/// cross-backend determinism fingerprint folds (see [`fnv_fold_cells`](crate::util::fnv_fold_cells)).
pub(in crate::generate) fn cells_u8(digits: &DigitGrid) -> [u8; CELLS] {
    core::array::from_fn(|i| digits.get(i).map_or(0, |d| d.get()))
}

/// Whether the [`FusedLogicSolver`] fast path is valid for `spec`'s baseline gate —
/// bb's strategy dispatch on the `forced` mask, lifted to the solve layer. The fused
/// closure does naked + hidden singles always and both LC orientations together, and
/// records the cheap kinds (singles, locked candidates) fired-or-not rather than by
/// exact count. So it is sound only when the baseline has **both singles**, LC
/// **both-or-neither**, and **no Forced cheap kind** (whose exact count the requirement
/// check would read). The production train/drill(HiddenQuad) specs satisfy this; a spec
/// that forces a cheap kind (e.g. train(LcPointing)) must use the exact [`LogicSolver`],
/// exactly as bb routes a forced cheap kind off its fused closure.
pub(in crate::generate) fn baseline_fast_applicable(spec: &Spec) -> bool {
    const NS: KindMask = 1 << NAKED_SINGLE;
    const HS: KindMask = 1 << HIDDEN_SINGLE;
    const LCP: KindMask = 1 << LC_POINTING;
    const LCC: KindMask = 1 << LC_CLAIMING;
    const CHEAP: KindMask = NS | HS | LCP | LCC;
    let baseline = spec.baseline_mask();
    let both_singles = baseline & NS != 0 && baseline & HS != 0;
    let lc_both_or_neither = (baseline & LCP != 0) == (baseline & LCC != 0);
    let no_forced_cheap = spec.forced_mask() & CHEAP == 0;
    both_singles && lc_both_or_neither && no_forced_cheap
}

/// True iff `digits` satisfies `spec`: baseline-solvable AND every Forced technique is
/// irreplaceable. The cold accept check, so it runs the composable [`LogicSolver`]
/// (exact, no fast-path precondition).
/// It runs on the **cell-major** [`Board`], not the strip's digit-major board: verify's
/// technique scans read candidates per cell, O(1) on `Board` vs a 9-board scan per `get`
/// on `SearchState`, which matters because the avoid-target walk re-solves repeatedly.
pub fn verify(digits: &DigitGrid, spec: &Spec) -> bool {
    let board = Board::from_digits(digits);
    // Positive: the baseline toolbox alone must solve it.
    if !LogicSolver::solve_tracked(&board, spec.baseline_mask()).solved {
        return false;
    }
    // Forcing: each Forced technique must beat the rest of the in-scope toolbox.
    let scope = spec.in_scope_mask();
    for (idx, need) in spec.forced() {
        if LogicSolver::min_target_uses(&board, scope, 1 << idx) < need as usize {
            return false;
        }
    }
    true
}

/// The mutable state of one strip attempt and its per-cell gate logic. The single
/// candidate source is one incrementally
/// maintained dual-banded board (`dual`) + its row-major `clue` map, mutated in place
/// across the 81 removal attempts (bb's `apply_clear`/`apply_place`) — so the uniqueness
/// prober reads its row view and the baseline gate reads both views with **no per-gate
/// rebuild**, exactly as bb reuses one dual `BitBoard`. `digits` is the placements shadow
/// the produced puzzle reads (and the strip's `alts==0`/`get` source `clue` can't give).
pub(in crate::generate) struct StripState {
    digits: DigitGrid,
    dual: DualBandedMarkGrid,
    clue: PerDigit<RM>,
    /// The most-stripped requirement-meeting grid; the candidate state is rebuilt from
    /// it (by `verify`) only if the attempt succeeds.
    pub(in crate::generate) best: Option<DigitGrid>,
    /// The running requirement verdict of the current accepted board (carried across
    /// the `alts == 0` fast path). The full grid fires nothing, so it starts false.
    req_met: bool,
}

impl StripState {
    /// Fresh state stripping the full `solution` grid (nothing removed yet). The one
    /// `from_digits` of the whole attempt — the strip mutates `dual` in place thereafter.
    pub(in crate::generate) fn new(solution: &Solution) -> Self {
        let digits = solution.0.clone();
        let dual = DualBandedMarkGrid::from_digits(&digits);
        let clue = DualBandedMarkGrid::clue_map(&digits);
        StripState { digits, dual, clue, best: None, req_met: false }
    }

    /// Revert a rejected strip of `cell` (held `orig`): restore the clue in both the
    /// placements shadow and the incremental candidate board (both views).
    fn revert(&mut self, cell: usize, orig: Digit) {
        self.digits.set(cell, orig);
        self.dual.place_clue(&mut self.clue, cell, orig);
    }

    /// Speculatively strip `cell` (holding `orig`): clear it from the placements shadow
    /// and the incremental dual board, returning the ALTERNATE digits the cell could
    /// still take as a 9-bit mask (`0` = still a naked single, so the strip is trivially
    /// valid). The warp host's per-lane resumable twin of [`attempt`]'s inline strip step
    /// — the gate logic stays in this one place so the sequential and SIMT drivers can't
    /// drift. `cand.without(orig)` is the prober's restriction (forbid `orig`); `0` of it
    /// is the `alts == 0` fast path.
    pub(in crate::generate) fn strip(&mut self, cell: usize, orig: Digit) -> u16 {
        self.digits.clear(cell);
        let cand = self.dual.clear_clue(&mut self.clue, cell, orig);
        debug_assert!(
            self.dual == DualBandedMarkGrid::from_digits(&self.digits),
            "incremental dual drift after clear at {cell}"
        );
        cand.without(Mark::single(orig)).bits()
    }

    /// `alts == 0` fast path (see [`attempt`]): the cleared cell is still a naked single,
    /// so both gates are skippable — just carry the running requirement verdict into
    /// `best`. The warp host's twin of the inline fast path.
    pub(in crate::generate) fn keep_trivial(&mut self) {
        if self.req_met {
            self.best = Some(self.digits.clone());
        }
    }

    /// The row-view candidate bands + empty mask as the packed prober's SoA input
    /// ([`crate::probe::simt::Probe`]'s `r`/`unsolved`) — bb's `export_r` twin: per digit
    /// its three 27-bit row-major bands ([`Bands::to_lanes`]), plus the still-empty mask.
    /// The carried `dual` is read directly — no per-gate rebuild.
    /// The digit currently at `cell` in the placements shadow, or `None` if it has been
    /// stripped — the warp host's strip-walk reads it to skip already-removed cells (the
    /// `digits.get` of [`attempt`]'s loop).
    pub(in crate::generate) fn digit_at(&self, cell: usize) -> Option<Digit> {
        self.digits.get(cell)
    }

    pub(in crate::generate) fn export_r(&self) -> ([[u32; 4]; 9], [u32; 4]) {
        let row = self.dual.row();
        let cand = row.candidates();
        (core::array::from_fn(|e| cand.each()[e].to_lanes()), row.unsolved().to_lanes())
    }

    /// Resolve the gates for the strip of `cell` (held `orig`) given the already-decided
    /// uniqueness verdict `nonunique`: if non-unique, revert; else run the baseline gate
    /// and either accept (updating `req_met`/`best`) or revert. `baseline` is the toolbox
    /// mask and `fast` selects the fused fast path vs the exact engine (see
    /// [`baseline_fast_applicable`]). The baseline reads the incrementally-maintained
    /// `dual` directly — no per-gate rebuild — and the solver clones it internally, so
    /// the carried strip state is untouched.
    pub(in crate::generate) fn resolve_gate(
        &mut self,
        cell: usize,
        orig: Digit,
        nonunique: bool,
        spec: &Spec,
        baseline: KindMask,
        fast: bool,
    ) {
        if nonunique {
            self.revert(cell, orig);
            return;
        }
        // The baseline view is the incrementally-maintained `dual` itself — no rebuild.
        let trace = if fast {
            FusedLogicSolver::solve_tracked(&self.dual, baseline)
        } else {
            LogicSolver::solve_tracked(&self.dual, baseline)
        };
        if !trace.solved {
            self.revert(cell, orig);
            return;
        }
        self.req_met = spec.requirement_met(&trace.counts);
        if self.req_met {
            self.best = Some(self.digits.clone());
        }
    }
}

/// One full strip attempt for `spec`. Mirrors the old bb `generator`'s strip-attempt gate for
/// gate, on the new prober/solver stack.
pub fn attempt(rng: &mut Rng, spec: &Spec) -> AttemptResult {
    let baseline = spec.baseline_mask();
    let fast = baseline_fast_applicable(spec);
    let solution = random_solution(rng);
    // Strip order — the 81 cell indices shuffled; a fixed stack array, no per-attempt
    // heap alloc. Same shuffle as bb, so the RNG stream and produced puzzle match.
    let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
    rng.shuffle(&mut positions);

    let mut st = StripState::new(&solution);
    for cell in positions {
        let Some(orig) = st.digits.get(cell) else {
            continue;
        };
        st.digits.clear(cell);
        let cand = st.dual.clear_clue(&mut st.clue, cell, orig);
        debug_assert!(
            st.dual == DualBandedMarkGrid::from_digits(&st.digits),
            "incremental dual drift after clear at {cell}"
        );
        // `alts == 0` fast path: the cleared cell is still a naked single, so its peers
        // already force `orig` — the strip stays unique AND baseline-solvable and the
        // requirement verdict is unchanged. Skip both gates; just carry `req_met`.
        if cand.len() == 1 {
            if st.req_met {
                st.best = Some(st.digits.clone());
            }
            continue;
        }
        // Uniqueness gate: forbid `orig` to restrict the cell to its alternates and ask
        // a single existence query (bb's `any_alt_solves`). The probe runs on a clone of
        // the dual's row view so the carried board stays the clue-only naked-candidate
        // state.
        let mut probe = st.dual.row().clone();
        probe.forbid(cell, orig);
        let nonunique = P::has_completion(probe);
        st.resolve_gate(cell, orig, nonunique, spec, baseline, fast);
    }

    match st.best {
        Some(snap) => {
            if verify(&snap, spec) {
                let givens = snap.digit_count();
                AttemptResult::Success(GeneratedPuzzle { puzzle: Puzzle(snap), solution, givens })
            } else {
                AttemptResult::NotForced
            }
        }
        None => AttemptResult::NeverFired,
    }
}

/// Per-run tallies, for throughput/yield reporting and the warp-vs-sequential
/// equivalence cross-check.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub attempts: usize,
    pub successes: usize,
    pub never_fired: usize,
    pub not_forced: usize,
    pub total_givens: usize,
}

impl Stats {
    /// Fold another tally into this one — the warp aggregates per-lane `Stats`.
    pub fn add(&mut self, o: &Stats) {
        self.attempts += o.attempts;
        self.successes += o.successes;
        self.never_fired += o.never_fired;
        self.not_forced += o.not_forced;
        self.total_givens += o.total_givens;
    }
}

/// Generate until a puzzle satisfying `spec` is found or `max_attempts` is hit.
pub fn generate(rng: &mut Rng, spec: &Spec, max_attempts: usize) -> (Option<GeneratedPuzzle>, Stats) {
    let mut stats = Stats::default();
    for _ in 0..max_attempts {
        stats.attempts += 1;
        match attempt(rng, spec) {
            AttemptResult::Success(p) => {
                stats.successes += 1;
                stats.total_givens += p.givens;
                return (Some(p), stats);
            }
            AttemptResult::NotForced => stats.not_forced += 1,
            AttemptResult::NeverFired => stats.never_fired += 1,
        }
    }
    (None, stats)
}

/// Run exactly `n` attempts (NOT "until found") for `spec` — a fixed-work, deterministic
/// benchmark of the per-attempt cost mix. Returns the tallies plus a fingerprint over
/// produced puzzles folded identically to the old bb `generator`'s `run_attempts`, so the two
/// fps are directly comparable.
pub fn run_attempts(rng: &mut Rng, spec: &Spec, n: usize) -> (Stats, u64) {
    let mut stats = Stats::default();
    let mut fp: u64 = FNV_OFFSET;
    for _ in 0..n {
        stats.attempts += 1;
        match attempt(rng, spec) {
            AttemptResult::Success(p) => {
                stats.successes += 1;
                stats.total_givens += p.givens;
                fnv_fold_cells(&mut fp, &cells_u8(&p.puzzle.0));
            }
            AttemptResult::NotForced => {
                stats.not_forced += 1;
                fp = fp.wrapping_mul(FNV_PRIME);
            }
            AttemptResult::NeverFired => {
                stats.never_fired += 1;
                fp = fp.wrapping_mul(FNV_PRIME);
            }
        }
    }
    (stats, fp)
}

/// Cross-backend determinism fingerprint over `n` attempts' worth of the RNG stream.
/// Unlike [`run_attempts`]'s fp (which only folds *successful* puzzles, so it is blind to
/// the grids when nothing succeeds), this folds the full solution grid AND the shuffled
/// strip order of every iteration — the two and only two RNG consumers of an attempt (the
/// prober and baseline take no RNG). Native and wasm32 MUST return the same value; that is
/// the guard that the Lemire `range`/shuffle and the fill are target-independent. It walks
/// the identical RNG trajectory as `n` attempts (the strip consumes no RNG), so it is a
/// faithful probe. This is a correctness guard, not a perf metric. The digit fold via
/// [`cells_u8`] is pinned per seed in `tests/faithful` (and cross-checked by the wasm
/// `det_fp` export).
pub fn determinism_fp(rng: &mut Rng, n: usize) -> u64 {
    let mut fp: u64 = FNV_OFFSET;
    for _ in 0..n {
        let cells = cells_u8(&random_solution(rng).0);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        for i in 0..CELLS {
            fp ^= cells[i] as u64;
            fp = fp.wrapping_mul(FNV_PRIME);
            fp ^= positions[i] as u64;
            fp = fp.wrapping_mul(FNV_PRIME);
        }
    }
    fp
}
