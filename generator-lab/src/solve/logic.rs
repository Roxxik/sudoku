//! `LogicSolver` — the **composable** reference logic solver: drives the full
//! discrete technique ladder ([`techniques`](super::techniques)) over any
//! [`LogicBoard`], easiest-first, never backtracking. Correct, packing-agnostic, and
//! the kept default — the analogue of [`probe::Singles`](crate::probe::Singles) for
//! the prober. The fused per-band fast path [`FusedLogicSolver`](super::FusedLogicSolver)
//! slots in behind the same [`Solver`] surface; this stays the fallback and the
//! correctness oracle.

use super::{LogicBoard, Solver, techniques};
use crate::repr::{CELLS, CellIdx, Digit, UNITS};
use crate::rng::Rng;
use crate::spec::kinds::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, KindMask, LC_CLAIMING, LC_POINTING,
    NAKED_PAIR, NAKED_QUAD, NAKED_SINGLE, NAKED_TRIPLE, NUM, SolveTrace,
};

/// The composable logic solver: drives the full discrete technique ladder
/// ([`techniques`](super::techniques)) over any [`LogicBoard`], easiest-first. The
/// reference engine — correct, packing-agnostic, and the kept default; the analogue
/// of [`probe::Singles`](crate::probe::Singles).
pub struct LogicSolver;

impl<V: LogicBoard> Solver<V> for LogicSolver {
    fn solve_tracked(board: &V, allowed: KindMask) -> SolveTrace {
        let mut b = board.clone();
        let mut counts = [0u16; NUM];
        let bat_singles = allowed & (1 << NAKED_SINGLE) != 0;
        loop {
            // Naked singles drain in batched waves (the overwhelmingly most common
            // step, and the cascade after every harder elimination) instead of one
            // per full ladder scan. This only reorders placements within the naked-
            // single band, so it is confluent with the easiest-first trace.
            if bat_singles {
                let n = drain_naked_singles(&mut b);
                if n > 0 {
                    counts[NAKED_SINGLE] = counts[NAKED_SINGLE].saturating_add(n);
                    continue;
                }
            }
            if is_solved(&b) {
                return SolveTrace { solved: true, counts };
            }
            // Mask naked single off once drained — a fresh full scan for it is
            // guaranteed to find nothing.
            let rest = if bat_singles { allowed & !(1 << NAKED_SINGLE) } else { allowed };
            match step_once(&mut b, rest) {
                Some(idx) => counts[idx] = counts[idx].saturating_add(1),
                None => return SolveTrace { solved: false, counts },
            }
        }
    }

    fn min_target_uses(board: &V, scope: KindMask, target: KindMask) -> usize {
        let mut b = board.clone();
        let non_target = scope & !target;
        let mut count = 0usize;
        loop {
            if is_solved(&b) {
                return count;
            }
            if step_once(&mut b, non_target).is_some() {
                continue;
            }
            // Stuck on non-targets: the only steps `scope` adds are target ones.
            match step_once(&mut b, scope) {
                None => return count, // stuck entirely
                Some(idx) => {
                    if target & (1 << idx) != 0 {
                        count += 1;
                    }
                }
            }
        }
    }
}

/// Apply the first in-`allowed` technique that fires, returning its kind index, or
/// `None` if none apply (stuck). The try-order below is THIS engine's choice (it
/// happens to follow core's order) — the kind index does not dictate it, and other
/// engines may order differently. NOTE: the fish ladder order is an un-optimized
/// placeholder (core's order); it is not benchmarked yet. Branch-scoped specs never
/// have both subsets and fish in scope at once, so their relative order is moot for
/// production; only an all-techniques baseline would see it. The same holds for the
/// bivalue wings (XY-/XYZ-/W-Wing), appended last.
fn step_once<V: LogicBoard>(b: &mut V, allowed: KindMask) -> Option<usize> {
    macro_rules! try_kind {
        ($bit:expr, $call:expr) => {
            if allowed & (1 << $bit) != 0 && $call {
                return Some($bit);
            }
        };
    }
    try_kind!(NAKED_SINGLE, techniques::naked_single(b));
    try_kind!(HIDDEN_SINGLE, techniques::hidden_single(b));
    try_kind!(LC_POINTING, techniques::lc_pointing(b));
    try_kind!(LC_CLAIMING, techniques::lc_claiming(b));
    try_kind!(NAKED_PAIR, techniques::naked_subset(b, 2, None, None));
    try_kind!(HIDDEN_PAIR, techniques::hidden_subset(b, 2, None, None));
    try_kind!(NAKED_TRIPLE, techniques::naked_subset(b, 3, None, None));
    try_kind!(HIDDEN_TRIPLE, techniques::hidden_subset(b, 3, None, None));
    try_kind!(NAKED_QUAD, techniques::naked_subset(b, 4, None, None));
    try_kind!(HIDDEN_QUAD, techniques::hidden_subset(b, 4, None, None));
    if let Some(k) = techniques::fish_step(b, allowed, None) {
        return Some(k);
    }
    if let Some(k) = techniques::wing_step(b, allowed) {
        return Some(k);
    }
    None
}

/// Place every naked single in repeated waves until none remain; return how many
/// were placed. Placing during a sweep lets a single created later in the same pass
/// be caught immediately; the outer loop catches ones created earlier.
fn drain_naked_singles<V: LogicBoard>(b: &mut V) -> u16 {
    let mut placed = 0u16;
    loop {
        if !techniques::naked_single(b) {
            return placed;
        }
        placed = placed.saturating_add(1);
        // naked_single placed exactly one; re-scan from the top for cascades.
    }
}

/// Whether every cell is filled — the solved verdict the easiest-first loop checks
/// once no naked single drains.
#[inline]
fn is_solved<V: LogicBoard>(b: &V) -> bool {
    (0..CELLS).all(|c| b.is_occupied(c))
}

// --- confluence-test support --------------------------------------------------
//
// The strip loop's keep/required decisions are *fixpoint* properties (solvable? still
// stuck without the forced kind?), so they must not depend on the order the solver
// tries techniques. Singles and locked candidates are monotone (their firing condition
// survives any elimination), hence trivially confluent; the subsets/fish/wings are the
// only ones whose order could matter — the wings especially, whose bivalue/trivalue
// condition is non-monotone. `examples/confluence.rs` replays generated puzzles' solves
// under permutations of exactly this harder block and asserts the fixpoint is invariant.

/// One harder-than-closure technique group, as a value so the confluence harness can
/// reorder the ladder. Production drives [`HARD_STEPS_DEFAULT`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardStep {
    /// Naked subset of the given size (2 pair, 3 triple, 4 quad).
    NakedSubset(u8),
    /// Hidden subset of the given size.
    HiddenSubset(u8),
    /// The basic fish (X-Wing / Swordfish / Jellyfish), each gated by `allowed`.
    Fish,
    /// The bivalue wings (XY- / XYZ- / W-Wing), each gated by `allowed`.
    Wings,
}

/// The production harder-ladder order, identical to [`step_once`]'s tail: subsets by
/// ascending size (naked before hidden), then the fish, then the bivalue wings.
pub const HARD_STEPS_DEFAULT: [HardStep; 8] = [
    HardStep::NakedSubset(2),
    HardStep::HiddenSubset(2),
    HardStep::NakedSubset(3),
    HardStep::HiddenSubset(3),
    HardStep::NakedSubset(4),
    HardStep::HiddenSubset(4),
    HardStep::Fish,
    HardStep::Wings,
];

/// Apply the first firing harder step from `order`, honouring `allowed`; returns its
/// kind index, or `None` if none fire. Singles and LC are assumed already drained by the
/// caller's closure (so this is exactly `step_once`'s tail, made reorderable).
fn step_harder_ordered<V: LogicBoard>(
    b: &mut V,
    allowed: KindMask,
    order: &[HardStep],
) -> Option<usize> {
    for &step in order {
        let fired = match step {
            HardStep::NakedSubset(s) => {
                let bit = match s {
                    2 => NAKED_PAIR,
                    3 => NAKED_TRIPLE,
                    _ => NAKED_QUAD,
                };
                (allowed & (1 << bit) != 0 && techniques::naked_subset(b, s as usize, None, None))
                    .then_some(bit)
            }
            HardStep::HiddenSubset(s) => {
                let bit = match s {
                    2 => HIDDEN_PAIR,
                    3 => HIDDEN_TRIPLE,
                    _ => HIDDEN_QUAD,
                };
                (allowed & (1 << bit) != 0 && techniques::hidden_subset(b, s as usize, None, None))
                    .then_some(bit)
            }
            HardStep::Fish => techniques::fish_step(b, allowed, None),
            HardStep::Wings => techniques::wing_step(b, allowed),
        };
        if let Some(k) = fired {
            return Some(k);
        }
    }
    None
}

/// The easiest-first closure prefix below the harder block: hidden single, then the two
/// locked-candidates. Naked single is drained separately (in waves) by the caller.
/// Returns whether anything fired.
fn step_prefix<V: LogicBoard>(b: &mut V, allowed: KindMask) -> bool {
    macro_rules! try_kind {
        ($bit:expr, $call:expr) => {
            if allowed & (1 << $bit) != 0 && $call {
                return true;
            }
        };
    }
    try_kind!(HIDDEN_SINGLE, techniques::hidden_single(b));
    try_kind!(LC_POINTING, techniques::lc_pointing(b));
    try_kind!(LC_CLAIMING, techniques::lc_claiming(b));
    false
}

/// Solve `board` to the `allowed`-toolbox fixpoint with the harder ladder tried in
/// `order`, returning the final board (solved or stuck). The loop shape matches
/// [`solve_tracked`](LogicSolver::solve_tracked) — naked singles drain in waves, then
/// one prefix-or-harder step, repeat — so with `order == HARD_STEPS_DEFAULT` it reaches
/// the same fixpoint the production solver does; other orders are the confluence probe.
/// The returned board's placements + candidates ARE the fixpoint the harness compares.
pub fn solve_fixpoint_with_order<V: LogicBoard>(
    board: &V,
    allowed: KindMask,
    order: &[HardStep],
) -> V {
    let mut b = board.clone();
    let bat_singles = allowed & (1 << NAKED_SINGLE) != 0;
    loop {
        if bat_singles && drain_naked_singles(&mut b) > 0 {
            continue;
        }
        if is_solved(&b) {
            return b;
        }
        let rest = if bat_singles { allowed & !(1 << NAKED_SINGLE) } else { allowed };
        if step_prefix(&mut b, rest) {
            continue;
        }
        if step_harder_ordered(&mut b, rest, order).is_some() {
            continue;
        }
        return b; // stuck: no allowed technique fires
    }
}

// --- grading solve (cold path) ------------------------------------------------
//
// `solve_graded` is the campaign difficulty grader's instrumented easiest-first solve
// (see `docs/campaign-grader-plan.md`). It is additive and runs ONLY on already-valid
// yielded puzzles — firmly off the hot generation path — so it can afford the richer
// bookkeeping `solve_tracked` deliberately omits. It mirrors `solve_tracked`'s ladder
// exactly (same cheap closure, same `HARD_STEPS_DEFAULT` harder order), so it reaches
// the identical `solved` verdict and the identical sequence of *harder* steps; the only
// difference is what it records along the way.

/// The cheap-closure toolbox: naked/hidden singles + both locked-candidate orientations
/// (kind indices `0..4`). The grading solve drains these to a fixpoint between harder
/// steps; what the closure places is the "free" progress whose presence marks a firing
/// as *paid-off* (and whose absence makes it *dry* — signal 2). The boundary doubles as
/// the harder-step boundary: every kind `>= NAKED_PAIR` is a harder step.
pub const CHEAP_KINDS: KindMask =
    (1 << NAKED_SINGLE) | (1 << HIDDEN_SINGLE) | (1 << LC_POINTING) | (1 << LC_CLAIMING);

/// One harder-than-the-cheap-closure step of the grading solve, with the per-step facts the
/// grader's signals are defined over (the cheap closure that precedes the first step and
/// follows every step is NOT recorded as a step — only the harder ladder is). The `open` /
/// `depth` / `cascade` trio are the *granular* stall signals (`docs/grader-granular-scoring.md`,
/// S5/S6/S8): continuous reads off the stall state that stay live even when a technique's
/// elimination count is structurally pinned (the wings always kill ~1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GradeStep {
    /// The kind that fired (always a harder step: `>= NAKED_PAIR`).
    pub kind: u8,
    /// Whether the cheap closure run immediately AFTER this step placed >= 1 cell.
    /// `false` = **dry**: the harder scan bought no placement, and another harder step
    /// is needed before any reward (signal 2). A harder step only ever eliminates, so a
    /// placement can come only from the following closure — this is exactly that test.
    pub paid_off: bool,
    /// How many candidate eliminations this firing made — the cheap first-cut proxy for
    /// signal 3 (scarcity) the plan specifies: the number of distinct eliminations the
    /// (first) forced firing makes at the stall. A harder step never places, so this is
    /// just the board's candidate-population drop across the step.
    pub elims: u16,
    /// **S5 (open)** — the live-candidate population on the board the instant this step had
    /// to fire ([`candidate_population`] *before* the step). The size of the search space at
    /// the stall: ~40..200, effectively continuous, so it discriminates puzzles a pinned
    /// `elims` cannot. A sparse board (few candidates left) is a different hunt from a busy one.
    pub open: u32,
    /// **S6 (depth)** — how many cells are already placed when this step fires
    /// ([`occupied_count`], unchanged by a harder step). A late stall means the cheap closure
    /// carried the solver a long way first; an early one means the puzzle resists immediately.
    pub depth: u16,
    /// **S8 (cascade)** — how many cells the cheap closure placed *immediately after* this
    /// step (the occupancy delta `paid_off` thresholds at >= 1; this is its magnitude). A
    /// firing that unlocks a long single-cascade is gentler than one that re-stalls at once.
    pub cascade: u16,
    /// **S7 (alts)** — total productive deduction instances across the allowed harder toolbox
    /// at this stall ([`count_alternatives`](super::techniques::count_alternatives)), counted
    /// non-mutating before the step fires. The honest scarcity: fewer alternatives = a tighter
    /// needle hunt = harder, and it stays live where `elims` is structurally pinned (the wings
    /// always kill ~1, but the number of eligible patterns still varies).
    pub alts: u32,
    /// **E1 (examined)** — total *structurally-valid* deduction instances across the allowed
    /// harder toolbox at this stall — pattern-shaped groups the solver must look at, productive
    /// or not (the `examined` half of [`count_alternatives`](super::techniques::count_alternatives)).
    /// `examined - alts` is the **near-miss camouflage** (`docs/grader-continuous-scoring.md` §5):
    /// look-alike patterns a solver spots and discards because they remove nothing — the spotting
    /// cost that stays live where `alts` is pinned (the wing residual).
    pub examined: u32,
}

/// What [`solve_graded`] returns: the [`SolveTrace`] facts (`solved` + per-kind `counts`)
/// plus the ordered harder-step sequence the grader's signals 2 and 3 read. The harder
/// (`>= NAKED_PAIR`) counts and `solved` match [`LogicSolver::solve_tracked`] exactly
/// (same path); the cheap counts may differ in tally (the closure drains in a different
/// interleaving) but are never read by the grader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GradeTrace {
    pub solved: bool,
    pub counts: [u16; NUM],
    pub steps: Vec<GradeStep>,
}

/// Total live candidates over the board: `sum of len()` across the still-empty cells.
/// The before/after delta across one harder step is its elimination count (a harder step
/// only prunes candidates, never places — so occupancy is unchanged and the whole drop is
/// eliminations).
fn candidate_population<V: LogicBoard>(b: &V) -> u32 {
    (0..CELLS).filter(|&c| b.is_empty(c)).map(|c| b.get(c).len()).sum()
}

/// How many cells hold a digit — the payoff yardstick: the cheap closure after a harder
/// step paid off iff this rose.
fn occupied_count<V: LogicBoard>(b: &V) -> usize {
    (0..CELLS).filter(|&c| b.is_occupied(c)).count()
}

/// One firing of the cheap closure prefix (hidden single, then both locked candidates,
/// and — only when naked single is NOT drained in waves — naked single), gated by
/// `cheap`. Returns the fired kind, mirroring [`step_once`]'s cheap head so the grading
/// solve walks the identical cheap closure.
fn cheap_step_once<V: LogicBoard>(b: &mut V, cheap: KindMask, bat_singles: bool) -> Option<usize> {
    macro_rules! try_kind {
        ($bit:expr, $call:expr) => {
            if cheap & (1 << $bit) != 0 && $call {
                return Some($bit);
            }
        };
    }
    // Naked single is drained separately in waves when allowed (matching `solve_tracked`);
    // include it here only for the rare toolbox that drained it differently.
    if !bat_singles {
        try_kind!(NAKED_SINGLE, techniques::naked_single(b));
    }
    try_kind!(HIDDEN_SINGLE, techniques::hidden_single(b));
    try_kind!(LC_POINTING, techniques::lc_pointing(b));
    try_kind!(LC_CLAIMING, techniques::lc_claiming(b));
    None
}

/// Drain the cheap closure (`cheap` toolbox) to a fixpoint, tallying each firing into
/// `counts`. Naked singles drain in waves (cheapest, the cascade after every elimination),
/// then the hidden-single / locked-candidate prefix one firing at a time — exactly
/// `solve_tracked`'s closure, just run to fixpoint in one call. Singles + locked candidates
/// are confluent, so the fixpoint board is the one every easiest-first engine reaches.
fn grade_cheap_closure<V: LogicBoard>(b: &mut V, cheap: KindMask, counts: &mut [u16; NUM]) {
    let bat_singles = cheap & (1 << NAKED_SINGLE) != 0;
    loop {
        if bat_singles {
            let n = drain_naked_singles(b);
            if n > 0 {
                counts[NAKED_SINGLE] = counts[NAKED_SINGLE].saturating_add(n);
                continue;
            }
        }
        match cheap_step_once(b, cheap, bat_singles) {
            Some(k) => counts[k] = counts[k].saturating_add(1),
            None => return,
        }
    }
}

/// The grading easiest-first solve: same fixpoint and harder ladder as
/// [`LogicSolver::solve_tracked`], recording a [`GradeStep`] per harder step (the
/// dry/payoff flag for signal 2 and the elimination count for signal 3's proxy). Cold
/// path only — runs on a yielded puzzle, never in the strip loop.
///
/// Shape: drain the cheap closure to its singles + locked-candidate fixpoint, then while
/// unsolved take ONE harder step (`HARD_STEPS_DEFAULT`, i.e. `step_once`'s tail), measure
/// its eliminations, re-run the cheap closure, and record whether that closure placed a
/// cell. A harder step that the closure does not reward is *dry*. `counts` tallies every
/// firing so signals 1 (bottleneck count) and 4 (scan work) read straight off it.
pub fn solve_graded<V: LogicBoard>(board: &V, allowed: KindMask) -> GradeTrace {
    let mut b = board.clone();
    let mut counts = [0u16; NUM];
    let mut steps = Vec::new();
    let cheap = allowed & CHEAP_KINDS;
    // Cheap closure to the first stall (no preceding harder step, so nothing to score).
    grade_cheap_closure(&mut b, cheap, &mut counts);
    loop {
        if is_solved(&b) {
            return GradeTrace { solved: true, counts, steps };
        }
        let before = candidate_population(&b);
        // S7 + E1: count the productive AND the total structurally-valid (examined) deductions
        // the toolbox offers at this stall BEFORE taking the forced step (non-mutating, so the
        // count reflects the stall, not the aftermath). `examined - productive` is the camouflage.
        let alts = techniques::count_alternatives(&b, allowed);
        let Some(kind) = step_harder_ordered(&mut b, allowed, &HARD_STEPS_DEFAULT) else {
            // Stuck: no harder step fires and the board is unsolved (baseline-unsolvable).
            return GradeTrace { solved: false, counts, steps };
        };
        counts[kind] = counts[kind].saturating_add(1);
        // A harder step only eliminates, so the candidate-population drop IS its
        // elimination count and occupancy is unchanged until the closure runs.
        let elims = before.saturating_sub(candidate_population(&b)).min(u16::MAX as u32) as u16;
        // Occupancy is invariant across the harder step (it only prunes), so this is both
        // the stall fill-depth (S6) and the payoff baseline for the cascade below.
        let occ_before = occupied_count(&b);
        grade_cheap_closure(&mut b, cheap, &mut counts);
        let occ_after = occupied_count(&b);
        let paid_off = occ_after > occ_before;
        let cascade = (occ_after - occ_before).min(u16::MAX as usize) as u16;
        steps.push(GradeStep {
            kind: kind as u8,
            paid_off,
            elims,
            open: before,
            depth: occ_before.min(u16::MAX as usize) as u16,
            cascade,
            alts: alts.productive,
            examined: alts.examined,
        });
    }
}

// --- trunk fill-path profile (docs/grader-external-calibration.md, Stage 2) -----------------
//
// A trunk puzzle is one no harder-than-cheap technique resolves — singles + locked candidates
// solve it outright. The per-technique grader has nothing to grade there (no bottleneck firing),
// so it rates every such puzzle a flat `0`; but real human-difficulty corpora concentrate in this
// range, and Pelánek's *Dependency* metric proves a singles-only puzzle still carries a continuous
// difficulty signal — the **frontier width** of the forced-fill chain (fewer simultaneous forced
// cells = a narrower, more sequential, harder chain). [`trunk_profile`] reproduces that signal off
// our own deterministic fill (not tuned to Pelánek), plus the locked-candidate structure the
// singles-only model lacks.

/// The trunk **fill-path profile**: the frontier-width curve of the singles + locked-candidate
/// closure — our deterministic analogue of Pelánek's *Dependency* — plus the locked-candidate
/// structure the singles-only model has no analogue for. Built by [`trunk_profile`] off one
/// easiest-first fill; read by [`grade::trunk_rating`](crate::grade::trunk_rating) to give the
/// trunk bucket a real within-bucket sub-order in place of the flat `0`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrunkProfile {
    /// `possibilities[i]` = how many cells were forced by a naked or hidden single (deduped by
    /// cell — the *frontier width*) the instant trunk step `i` had to pick a cell, **before** one
    /// was placed. A narrow frontier is a more sequential, harder chain; Pelánek's Dependency is
    /// the mean of exactly this, *inverse* to difficulty (larger = easier).
    pub possibilities: Vec<u32>,
    /// Locked-candidate elimination firings applied to break a singles stall — the one cheap
    /// technique strictly above singles. Higher = harder; the singles-only Dependency model has no
    /// analogue for it.
    pub lc_elims: u32,
    /// How many times the singles frontier emptied and a locked candidate was needed to resume —
    /// the "stalled and resumed" structure. Higher = harder.
    pub lc_stalls: u32,
    /// Whether singles + locked candidates finished the grid. Always `true` for a trunk-keyed
    /// puzzle (it solved under the full toolbox with no harder kind firing); the field keeps the
    /// featurizer total for any other input.
    pub solved: bool,
}

/// Enumerate the cells forced by a naked or hidden single on `b` as `(cell, digit)` fill moves —
/// the frontier, deduped by cell (a naked single's lone candidate is the only digit any hidden
/// single could target the cell with, so a cell is never double-counted, and either reading gives
/// the same forced digit). Naked singles first in cell order, then the remaining hidden singles in
/// unit order, so `out[0]` is the lowest-index naked single (else the first hidden single) — the
/// exact cell [`super::techniques::naked_single`]/[`super::techniques::hidden_single`] would place.
/// Non-mutating: it reads the stall, it does not resolve it.
///
/// `out.len()` is the frontier width — Pelánek's *Dependency* per-step `possibilities`. A
/// uniformly-random element is one randomized fill move ([`trunk_profiles_rand`]).
fn forced_moves<V: LogicBoard>(b: &V, out: &mut Vec<(CellIdx, Digit)>) {
    out.clear();
    let mut seen = [false; CELLS];
    // Naked singles: an empty cell with exactly one candidate.
    for c in 0..CELLS {
        if b.is_empty(c) {
            let m = b.get(c);
            if m.len() == 1 {
                out.push((c, m.iter().next().expect("exactly one candidate")));
                seen[c] = true;
            }
        }
    }
    // Hidden singles: a (unit, digit) whose digit has exactly one candidate cell.
    for unit in &UNITS {
        for di in 0..9 {
            let d = Digit::from_index(di);
            let mut only = None;
            let mut count = 0u32;
            for &c in unit {
                if b.get(c).contains(d) {
                    only = Some(c);
                    count += 1;
                    if count > 1 {
                        break;
                    }
                }
            }
            if count == 1 {
                let c = only.expect("count == 1");
                if !seen[c] {
                    out.push((c, d));
                    seen[c] = true;
                }
            }
        }
    }
}

/// Profile one trunk fill of `board`: an easiest-first singles + locked-candidate solve that, at
/// every step, records the [`forced_moves`] frontier width *before* placing **one** forced cell —
/// the cell `pick(frontier_len)` selects — and tallies the locked-candidate firings needed to break
/// each singles stall. Cold path; runs on a trunk-keyed dataset puzzle off the hot loop. See
/// [`TrunkProfile`].
///
/// `pick` chooses which of the `frontier_len` forced cells to place: the deterministic baseline
/// ([`trunk_profile`]) passes `|_| 0` (`out[0]` = the lowest-index naked-first move, `step_once`'s
/// cheap order); a randomized run ([`trunk_profiles_rand`]) passes `|len| rng.range(len)` (Pelánek's
/// uniform simple-move pick). The frontier-width *sequence* depends on the order, which is the whole
/// point of averaging over orders — but the **number of placements is invariant** (one per empty
/// cell), so every run records the same number of `possibilities` and the curves are averageable per
/// step index ([`grade::trunk_dependency_avg`](crate::grade::trunk_dependency_avg)).
///
/// Both progress measures are monotone (a placement fills a cell, a locked candidate prunes a
/// candidate), so the fill always terminates; on a genuine trunk puzzle it terminates *solved*.
fn trunk_fill<V: LogicBoard>(board: &V, mut pick: impl FnMut(usize) -> usize) -> TrunkProfile {
    let mut b = board.clone();
    let mut p = TrunkProfile::default();
    let mut moves: Vec<(CellIdx, Digit)> = Vec::new();
    loop {
        if is_solved(&b) {
            p.solved = true;
            return p;
        }
        forced_moves(&b, &mut moves);
        if !moves.is_empty() {
            p.possibilities.push(moves.len() as u32);
            let (c, d) = moves[pick(moves.len())];
            b.place(c, d);
            continue;
        }
        // Singles stalled: apply locked-candidate firings until a single reappears (or none
        // applies). Each `lc_*` call applies one firing's eliminations; loop so a chain of locked
        // candidates that together unlock a single counts as ONE stall, not several.
        let mut fired = false;
        loop {
            if techniques::lc_pointing(&mut b) || techniques::lc_claiming(&mut b) {
                p.lc_elims += 1;
                fired = true;
                forced_moves(&b, &mut moves);
                if !moves.is_empty() {
                    break; // a single reappeared — resume the singles fill
                }
            } else {
                break; // no locked candidate applies
            }
        }
        if fired {
            p.lc_stalls += 1;
            continue;
        }
        // No single and no locked candidate: the puzzle needs a harder technique. This does not
        // happen on a trunk-keyed puzzle (singles + LC solved it under the full toolbox); keep the
        // profiler total by returning the partial profile (`solved == false`).
        return p;
    }
}

/// Profile the trunk fill **deterministically** — easiest-first, naked single before hidden single,
/// lowest index first (`step_once`'s cheap order). The Stage-2 baseline; [`trunk_profiles_rand`] is
/// the Stage-4 randomized-frontier-average refinement.
pub fn trunk_profile<V: LogicBoard>(board: &V) -> TrunkProfile {
    trunk_fill(board, |_| 0)
}

/// Profile `runs` **randomized** trunk fills, each placing a uniformly-random forced cell from the
/// frontier (RNG seeded from `seed`, so the result is reproducible). This is our analogue of
/// Pelánek's averaging the *Dependency* frontier curve over several randomized fill orders — the
/// Stage-4 refinement of `docs/grader-external-calibration.md` §4.5, recovering the
/// determinism-vs-averaging margin our single deterministic fill leaves on the table (~80-95% of
/// Pelánek's magnitude). Every run solves the same trunk puzzle in the same number of placements, so
/// the profiles share a step count and
/// [`grade::trunk_dependency_avg`](crate::grade::trunk_dependency_avg) averages them per step index.
pub fn trunk_profiles_rand<V: LogicBoard>(board: &V, runs: usize, seed: u64) -> Vec<TrunkProfile> {
    let mut rng = Rng::from_seed(seed);
    let mut out = Vec::with_capacity(runs);
    for _ in 0..runs {
        out.push(trunk_fill(board, |len| rng.range(len)));
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::repr::banded::{Bands, RowMajor};
    use crate::repr::{DigitGrid, FlatGridMask, Marks, SolverState};
    use crate::solve::{LogicSolver, Solver};
    use crate::subset_spec_for_mode;

    type Banded = SolverState<Bands<RowMajor>>;

    const PUZZLE: &str = "\
        53..7....\
        6..195...\
        .98....6.\
        8...6...3\
        4..8.3..1\
        7...2...6\
        .6....28.\
        ...419..5\
        ....8..79";

    fn state<M: crate::repr::GridMask>(s: &str) -> SolverState<M> {
        SolverState::from_digits(&DigitGrid::parse(s).unwrap())
    }

    /// The classic singles puzzle solves with just the two singles, and the verdict
    /// is independent of packing (flat vs banded reach the same fixpoint).
    #[test]
    fn solves_singles_puzzle() {
        let baseline = subset_spec_for_mode(0).baseline_mask();
        let flat = LogicSolver::solve_tracked(&state::<FlatGridMask>(PUZZLE), baseline);
        let band = LogicSolver::solve_tracked(&state::<Bands<RowMajor>>(PUZZLE), baseline);
        assert!(flat.solved, "did not solve under train baseline");
        assert_eq!(flat.solved, band.solved);
        assert_eq!(flat.counts, band.counts, "packing changed the trace");
    }

    /// A board that is not solvable by the toolbox reports `solved == false` rather
    /// than looping — the empty grid has many completions, so singles stall.
    #[test]
    fn unsolvable_stops() {
        let baseline = subset_spec_for_mode(0).baseline_mask();
        let empty: Banded = state(&".".repeat(81));
        assert!(!LogicSolver::solve_tracked(&empty, baseline).solved);
    }

    /// The trunk profiler fills the classic singles puzzle to a solved grid, recording one
    /// frontier width per placed cell — the continuous fill-path signal Stage 2 grades on.
    #[test]
    fn trunk_profile_records_frontier_and_solves() {
        let p = super::trunk_profile(&state::<Bands<RowMajor>>(PUZZLE));
        assert!(p.solved, "the classic puzzle is solved by singles");
        // One frontier reading per non-given cell placed; every reading is a real forced count.
        assert!(!p.possibilities.is_empty(), "a singles fill records its frontier");
        assert!(p.possibilities.iter().all(|&w| w >= 1), "each step has >= 1 forced cell");
    }

    /// Every randomized fill order solves the same trunk puzzle in the **same number of
    /// placements** as the deterministic one — the invariant that lets the frontier curves be
    /// averaged per step index. The frontier *sequence* may differ (that is the point of
    /// averaging), but its length must not.
    #[test]
    fn trunk_profiles_rand_share_the_deterministic_step_count() {
        let board = state::<Bands<RowMajor>>(PUZZLE);
        let det = super::trunk_profile(&board);
        let runs = super::trunk_profiles_rand(&board, 8, 0xC0FFEE);
        assert_eq!(runs.len(), 8);
        for (i, r) in runs.iter().enumerate() {
            assert!(r.solved, "randomized run {i} solved the trunk puzzle");
            assert_eq!(
                r.possibilities.len(),
                det.possibilities.len(),
                "randomized run {i} placed the same number of cells as the deterministic fill",
            );
            assert!(r.possibilities.iter().all(|&w| w >= 1), "each step has >= 1 forced cell");
        }
    }

    /// An already-complete grid profiles to no steps (nothing to fill) and reports solved.
    #[test]
    fn trunk_profile_complete_grid_has_no_steps() {
        const SOLVED: &str = "\
            534678912\
            672195348\
            198342567\
            859761423\
            426853791\
            713924856\
            961537284\
            287419635\
            345286179";
        let p = super::trunk_profile(&state::<Bands<RowMajor>>(SOLVED));
        assert!(p.solved && p.possibilities.is_empty());
    }
}
