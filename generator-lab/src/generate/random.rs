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
//! replaced, plus the trajectory-invariant hidden-single re-force fast path
//! ([`StripState::reforced`], which skips gates whose verdict is implied), so for a
//! given seed it produces byte-identical puzzles (the warp still runs that bb strip;
//! `tests/equiv_warp` pins the two lane-for-lane).
//!
//! The candidate state is carried incrementally across the 81 strip steps exactly as
//! bb does: a single [`DualSolverState`] plus a `clue` map, reopened/reclosed in
//! place with [`clear_clue`](DualSolverState::clear_clue) /
//! [`place_clue`](DualSolverState::place_clue) rather than rebuilt each step. Holding
//! both bandings live means the uniqueness prober reads the row view and the baseline
//! gate reads both — with **no per-gate reconstruction**, just as bb reuses one dual
//! `BitBoard` for both gates. The lone `from_digits` is once per attempt, on the full
//! solution.

use crate::fill::random_solution;
use crate::probe::{Prober, Search};
use crate::repr::banded::{Bands, DualSolverState, RowMajor};
use crate::repr::{Board, CELLS, Digit, DigitGrid, GridMask, Mark, Marks, PerDigit, Puzzle, Solution, SolverState};
use crate::rng::Rng;
use crate::scan::Bivalue;
use crate::solve::{FusedLogicSolver, LogicSolver, Solver};
use crate::spec::Spec;
use crate::spec::kinds::{
    HIDDEN_SINGLE, KindMask, LC_CLAIMING, LC_POINTING, NAKED_SINGLE, SolveTrace,
};
use crate::fingerprint::{FNV_OFFSET, FNV_PRIME, fnv_fold_cells};

/// The banded packing the prober branches on and the incremental strip is carried in.
type RM = Bands<RowMajor>;
/// The uniqueness prober: the scan/sieve [`Search`] with the [`Bivalue`] branch
/// strategy — the optimal prober strategy for the strip (see the prober memos).
type P = Search<Bivalue>;

/// The strip's candidate-state capability — the incremental clue board the 81 removal
/// attempts mutate in place, abstracted so each strip driver carries only the views it
/// reads. The scalar/wasm [`attempt`] strips on the dual-banded [`DualSolverState`]
/// (its scalar baseline gate reads both views), while the SIMT host strips on a single
/// row-major [`SolverState`]: the warp consumes only row bands ([`StripState::export_r`]),
/// the prober and the gate tests are row-only, and the baseline trace comes back from
/// the warp — so maintaining a column view per clue clear/restore there would be pure
/// waste (~half the strip's fixed band traffic). Both impls run the identical
/// clue-survival algebra, so every gate decision — and hence the trajectory and the
/// produced puzzles — is byte-identical across instantiations.
pub trait StripView: Marks + PartialEq {
    /// The fully-decided state (a complete grid's `from_digits`, built directly).
    fn solved() -> Self;
    /// The per-digit clue map seed (row-major; the survival logic reads it
    /// banding-independently).
    fn clue_map(digits: &DigitGrid) -> PerDigit<RM>;
    /// Remove the clue digit `d` at `cell`, reopening candidates in every carried
    /// view; returns the cell's naked candidates (the strip's `alts` source).
    fn clear_clue(&mut self, clue: &mut PerDigit<RM>, cell: usize, d: Digit) -> Mark;
    /// Restore the clue digit `d` at `cell` in every carried view (the strip's revert).
    fn place_clue(&mut self, clue: &mut PerDigit<RM>, cell: usize, d: Digit);
    /// The row-major view — what the prober/warp exports and the gate tests read.
    fn row(&self) -> &SolverState<RM>;
}

impl StripView for DualSolverState {
    fn solved() -> Self {
        DualSolverState::solved()
    }
    fn clue_map(digits: &DigitGrid) -> PerDigit<RM> {
        DualSolverState::clue_map(digits)
    }
    fn clear_clue(&mut self, clue: &mut PerDigit<RM>, cell: usize, d: Digit) -> Mark {
        DualSolverState::clear_clue(self, clue, cell, d)
    }
    fn place_clue(&mut self, clue: &mut PerDigit<RM>, cell: usize, d: Digit) {
        DualSolverState::place_clue(self, clue, cell, d)
    }
    fn row(&self) -> &SolverState<RM> {
        DualSolverState::row(self)
    }
}

impl StripView for SolverState<RM> {
    fn solved() -> Self {
        SolverState::solved()
    }
    fn clue_map(digits: &DigitGrid) -> PerDigit<RM> {
        SolverState::<RM>::clue_map(digits)
    }
    /// The dual's [`clear_clue`](DualSolverState::clear_clue) row half, verbatim minus
    /// the column view — NOT the generic [`SolverState::clear_clue`], whose blocked-cell
    /// walk goes through `any()`/`first()`/`cell()` (a cross-lane branch + a table load
    /// per visited clue, measurably slower; the dual replaced it with the per-lane
    /// scalar bit-walk below for exactly that reason, see its comment).
    fn clear_clue(&mut self, clue: &mut PerDigit<RM>, cell: usize, d: Digit) -> Mark {
        let r_cell = <RM as GridMask>::cell(cell);
        let r_peers = <RM as GridMask>::peers(cell);
        clue[d] &= !r_cell; // cell stops being a clue before peer occupancy is read

        // (1) cell -> empty; candidate digit `e` survives iff no present peer holds it.
        let mut cand = Mark::EMPTY;
        for e in 0..9 {
            let dig = Digit::from_index(e);
            if !(clue[dig] & r_peers).any() {
                cand.insert(dig);
            }
        }
        self.open_cell(r_cell, cand);

        // (2) reopen `d` on cell's empty peers outside the union of the other present
        // `d`-clues' peer masks — the per-lane scalar bit-walk (`cell = 27*lane + bit`).
        let mut r_blocked = <RM as GridMask>::EMPTY;
        let lanes = clue[d].to_lanes();
        for (lane, mut w) in lanes[..3].iter().copied().enumerate() {
            let base = lane * 27;
            while w != 0 {
                let q = base + w.trailing_zeros() as usize;
                w &= w - 1;
                r_blocked |= <RM as GridMask>::peers(q);
            }
        }
        self.open_digit_on_peers(d, r_peers, r_blocked);
        cand
    }
    fn place_clue(&mut self, clue: &mut PerDigit<RM>, cell: usize, d: Digit) {
        SolverState::place_clue(self, clue, cell, d)
    }
    fn row(&self) -> &SolverState<RM> {
        self
    }
}

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

/// Which unavoidable-set (UA) pre-filter library the strip carries — the deadly-pattern
/// filter of `docs/UA-FILTER.md`. Every UA of the solution grid must keep at least one
/// given or the puzzle is non-unique, so a strip that would empty a library UA is provably
/// a prober revert and can skip the probe entirely. Sound: it only ever fast-rejects gates
/// the prober would revert, so the strip trajectory — and hence the produced puzzles and
/// the `run_attempts` fingerprint — are bit-identical to [`UaTier::Off`]. The scalar and
/// SIMT paths may carry different tiers safely, since the verdicts are unchanged either way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UaTier {
    /// No pre-filter: every uniqueness gate poses a probe (the pre-filter baseline).
    Off,
    /// Size-4 UAs only (the deadly rectangles) — ~650 compares/board to build.
    Ua4,
    /// The full 2-digit-cycle UA library (sizes 4..=18).
    Full,
}

impl UaTier {
    /// Production tier for the scalar/wasm strip walk ([`attempt`], [`run_attempts`]).
    pub const SCALAR: UaTier = UaTier::Ua4;
    /// Production tier for the SIMT warp host ([`crate::generate::warp_host`]).
    pub const SIMT: UaTier = UaTier::Ua4;
}

/// Max UAs carried per board. The true maximum is 144 (36 digit pairs x <=4 components
/// each); the headroom lets a miscount degrade by truncation (sound: a dropped UA is only
/// a missed catch, never a wrong verdict) rather than panic. `u8` UA ids cap it at 255.
const UA_CAP: usize = 192;
/// Max UAs a single cell belongs to. A cell holding digit `d` joins exactly one component
/// per partner digit, so it is in at most 8 UAs (one per `{d, x}` pair) — an exact bound,
/// never truncated in practice.
const UA_PER_CELL: usize = 8;

/// The per-board UA pre-filter state ([`UaTier`]). Holds the library's `cell -> containing
/// UA` index plus a per-UA *given count*, maintained along the strip walk: a UA's count is
/// the number of its cells still given, starting at `|U|` (the walk begins from a full
/// board) and dropping by one each time one of its cells is kept-stripped. Stripping a cell
/// whose count is 1 would empty that UA — a provable non-unique — so the gate reverts
/// without a probe (see [`caught`](Self::caught)). Reverted cells stay givens, so their
/// count is untouched. All storage is fixed-capacity (no per-gate or per-attempt heap
/// allocation); [`UaTier::Off`] leaves it empty, so [`caught`](Self::caught) is always
/// `false` at zero cost.
struct UaFilter {
    /// Remaining given count per UA id (`0..nua`).
    counts: [u8; UA_CAP],
    /// `cell -> UA ids containing it`; the first `lens[cell]` entries are valid.
    cell_uas: [[u8; UA_PER_CELL]; CELLS],
    /// Number of valid entries in each `cell_uas` row.
    lens: [u8; CELLS],
    /// Number of UAs registered (`<= UA_CAP`).
    nua: usize,
}

impl UaFilter {
    const fn empty() -> Self {
        UaFilter {
            counts: [0; UA_CAP],
            cell_uas: [[0; UA_PER_CELL]; CELLS],
            lens: [0; CELLS],
            nua: 0,
        }
    }

    /// Build the `tier`'s UA library for the complete solution grid `sol`. The grid is
    /// flattened to a dense `[u8; 81]` once (the enumerations index it) — the banded
    /// [`DigitGrid::get`] is ~an order of magnitude pricier than an array read, and the
    /// enumerations touch hundreds of cells per board, so the flatten dominates the per-board
    /// cost.
    fn build(sol: &DigitGrid, tier: UaTier) -> Self {
        let mut f = UaFilter::empty();
        match tier {
            UaTier::Off => {}
            UaTier::Ua4 => f.enumerate_ua4(&Self::dense(sol)),
            UaTier::Full => f.enumerate_2digit(&Self::dense(sol)),
        }
        f
    }

    /// The complete solution as a dense row-major digit array (`0..=8`). A complete grid has
    /// every cell placed, so the `unwrap_or` is never taken.
    fn dense(sol: &DigitGrid) -> [u8; CELLS] {
        core::array::from_fn(|c| sol.get(c).map_or(0, |d| d.index() as u8))
    }

    /// Register a UA spanning `cells` (count initialized to `|cells|`). Truncation-safe:
    /// past [`UA_CAP`] the UA is dropped, and a cell already at [`UA_PER_CELL`] drops the
    /// membership — both only lose catches, never flip a verdict (neither happens for a
    /// valid 2-digit library).
    #[inline]
    fn push_ua(&mut self, cells: &[usize]) {
        if self.nua >= UA_CAP {
            return;
        }
        let id = self.nua as u8;
        self.counts[self.nua] = cells.len() as u8;
        self.nua += 1;
        for &c in cells {
            let n = self.lens[c] as usize;
            if n < UA_PER_CELL {
                self.cell_uas[c][n] = id;
                self.lens[c] = (n + 1) as u8;
            }
        }
    }

    /// Would stripping `cell` empty some library UA? `cell` is still a committed given at
    /// the gate, so a UA count of 1 means `cell` is that UA's sole remaining given —
    /// removing it leaves the UA givenless, i.e. an alternate completion exists (the
    /// non-unique the prober would revert). Sound: it never fires on a unique gate
    /// (measured `false_positive: 0`, M-UA-LIB).
    #[inline]
    fn caught(&self, cell: usize) -> bool {
        let n = self.lens[cell] as usize;
        self.cell_uas[cell][..n].iter().any(|&u| self.counts[u as usize] == 1)
    }

    /// Commit a kept strip of `cell`: it leaves the given set, so every UA containing it
    /// loses a given. Called on every keep (both fast paths and the baseline accept);
    /// reverts do not call it (the cell stays a given). The count is `>= 1` here (`cell`
    /// itself is one of those givens), so the decrement never underflows.
    #[inline]
    fn kept(&mut self, cell: usize) {
        let n = self.lens[cell] as usize;
        for i in 0..n {
            let u = self.cell_uas[cell][i] as usize;
            self.counts[u] -= 1;
        }
    }

    /// Enumerate the size-4 UAs (deadly rectangles) of the complete solution `g`. A UA4 is a
    /// rectangle whose `a<->b` swap is another valid grid: along one line pair the two cross
    /// lines hold the two digits in opposite order. In a valid grid this forces the rectangle
    /// to span exactly two boxes — the line pair shares a band/stack and the cross lines lie
    /// in different stacks/bands (a one-box rectangle would repeat a digit in that box, which
    /// the swap match then never satisfies). So every UA4 is found exactly once by scanning,
    /// for each of the 9 same-band row pairs (then 9 same-stack column pairs), the 9 cross
    /// lines: a cross line with digit signature `(x, y)` pairs with an earlier one of
    /// signature `(y, x)`. A generation-stamped table records each signature's cross line, so
    /// the scan is `O(9)` per line pair (no per-pair clear): 9*9 + 9*9 = 162 cell visits for
    /// ~10.7 UA4s/board (vs 648 for the brute-force rectangle scan; pinned equal by
    /// `ua4_equals_full_size4`). Note `x != y` always (two cells of one line differ), so a
    /// signature never collides with its own swap.
    fn enumerate_ua4(&mut self, g: &[u8; CELLS]) {
        // `seen[sig]` = the cross line carrying signature `sig`, valid iff `stamp[sig]` is the
        // current line pair's generation. `sig = x * 9 + y` for digits `x` (first line) and
        // `y` (second line).
        let mut seen = [0u8; 81];
        let mut stamp = [0u16; 81];
        let mut era = 0u16;
        let mut scan = |me: &mut Self, era: u16, cells: [[usize; 2]; 9]| {
            for (cross, &[p, q]) in cells.iter().enumerate() {
                let (x, y) = (g[p], g[q]);
                let swapped = (y * 9 + x) as usize;
                if stamp[swapped] == era {
                    let other = seen[swapped] as usize;
                    me.push_ua(&[cells[other][0], cells[other][1], p, q]);
                }
                let sig = (x * 9 + y) as usize;
                seen[sig] = cross as u8;
                stamp[sig] = era;
            }
        };
        // Same-band row pairs: line pair = rows (r1, r2), cross line = column c.
        for band in 0..3 {
            for i in 0..3 {
                for j in (i + 1)..3 {
                    let (r1, r2) = (band * 3 + i, band * 3 + j);
                    era += 1;
                    scan(self, era, core::array::from_fn(|c| [r1 * 9 + c, r2 * 9 + c]));
                }
            }
        }
        // Same-stack column pairs: line pair = columns (c1, c2), cross line = row r.
        for stack in 0..3 {
            for i in 0..3 {
                for j in (i + 1)..3 {
                    let (c1, c2) = (stack * 3 + i, stack * 3 + j);
                    era += 1;
                    scan(self, era, core::array::from_fn(|r| [r * 9 + c1, r * 9 + c2]));
                }
            }
        }
    }

    /// Enumerate the full 2-digit UA library of the complete solution `sol` (sizes 4..=18,
    /// superseding [`enumerate_ua4`](Self::enumerate_ua4)). Ported from the M-UA-LIB counter
    /// `enumerate_2digit_uas`: for each of the 36 digit pairs `{a, b}`, union-find over the
    /// 18 cells holding `a` or `b`, joining each unit's (row/column/box) two pair-cells; the
    /// connected components are the minimal UAs (an `a<->b` swap on any one is another valid
    /// grid). ~55 UAs/board. Fixed-capacity scratch — no per-board heap allocation.
    fn enumerate_2digit(&mut self, g: &[u8; CELLS]) {
        fn find(p: &mut [usize; CELLS], x: usize) -> usize {
            let mut r = x;
            while p[r] != r {
                r = p[r];
            }
            let mut c = x;
            while p[c] != r {
                let n = p[c];
                p[c] = r;
                c = n;
            }
            r
        }
        // The `k`-th cell of unit `u` of kind 0=row / 1=column / 2=box.
        let unit_cell = |kind: usize, u: usize, k: usize| -> usize {
            match kind {
                0 => u * 9 + k,
                1 => k * 9 + u,
                _ => ((u / 3) * 3 + k / 3) * 9 + ((u % 3) * 3 + k % 3),
            }
        };
        let mut parent = [0usize; CELLS];
        let mut cells = [0usize; 18];
        let mut roots: [(usize, u8); 18] = [(usize::MAX, 0); 18];
        for a in 0..9u8 {
            for b in (a + 1)..9u8 {
                for (c, p) in parent.iter_mut().enumerate() {
                    *p = c;
                }
                for kind in 0..3 {
                    for u in 0..9 {
                        // Each unit holds exactly one `a`-cell and one `b`-cell; join them.
                        let mut found = [usize::MAX; 2];
                        let mut n = 0;
                        for k in 0..9 {
                            let cell = unit_cell(kind, u, k);
                            if g[cell] == a || g[cell] == b {
                                if n < 2 {
                                    found[n] = cell;
                                }
                                n += 1;
                            }
                        }
                        if n == 2 {
                            let (ra, rb) = (find(&mut parent, found[0]), find(&mut parent, found[1]));
                            if ra != rb {
                                parent[ra] = rb;
                            }
                        }
                    }
                }
                // This pair's cells, then grouped by root into one UA per component.
                let mut ncells = 0;
                for cell in 0..CELLS {
                    if g[cell] == a || g[cell] == b {
                        cells[ncells] = cell;
                        ncells += 1;
                    }
                }
                let mut nroots = 0;
                for i in 0..ncells {
                    let cell = cells[i];
                    let r = find(&mut parent, cell);
                    let mut id = u8::MAX;
                    for &(rr, ri) in &roots[..nroots] {
                        if rr == r {
                            id = ri;
                            break;
                        }
                    }
                    if id == u8::MAX {
                        if self.nua >= UA_CAP {
                            continue; // truncation-safe (never reached: <=144 UAs/board)
                        }
                        id = self.nua as u8;
                        self.counts[self.nua] = 0;
                        self.nua += 1;
                        roots[nroots] = (r, id);
                        nroots += 1;
                    }
                    self.counts[id as usize] += 1;
                    let ln = self.lens[cell] as usize;
                    if ln < UA_PER_CELL {
                        self.cell_uas[cell][ln] = id;
                        self.lens[cell] = (ln + 1) as u8;
                    }
                }
            }
        }
    }
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
/// on `SolverState`, which matters because the avoid-target walk re-solves repeatedly.
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
/// candidate source is one incrementally maintained board (`state`, a [`StripView`])
/// + its row-major `clue` map, mutated in place
/// across the 81 removal attempts (bb's `apply_clear`/`apply_place`) — so the uniqueness
/// prober reads its row view (and the scalar baseline gate both views) with **no per-gate
/// rebuild**, exactly as bb reuses one dual `BitBoard`. `digits` is the placements shadow
/// the produced puzzle reads (and the strip's `alts==0`/`get` source `clue` can't give).
pub(in crate::generate) struct StripState<S: StripView = DualSolverState> {
    digits: DigitGrid,
    state: S,
    clue: PerDigit<RM>,
    /// The most-stripped requirement-meeting grid; the candidate state is rebuilt from
    /// it (by `verify`) only if the attempt succeeds.
    pub(in crate::generate) best: Option<DigitGrid>,
    /// The running requirement verdict of the current accepted board (carried across
    /// the `alts == 0` fast path). The full grid fires nothing, so it starts false.
    req_met: bool,
    /// The unavoidable-set pre-filter (see [`UaFilter`]); empty for [`UaTier::Off`].
    ua: UaFilter,
}

impl<S: StripView> StripState<S> {
    /// Fresh state stripping the full `solution` grid (nothing removed yet). The strip
    /// mutates `state` in place thereafter.
    ///
    /// `solution` is always a *complete* grid (the fill's output), and `from_digits` of a
    /// complete grid is the trivial solved state — every cell placed leaves no unsolved
    /// cells and no candidates — so [`StripView::solved`] builds it directly instead
    /// of running the 81-cell peer-clear loop (per carried view) only to clear nothing
    /// (~4% of the fill's cost, all of it wasted). The strip then reopens cells one at a
    /// time via [`StripView::clear_clue`]. `clue_map` still walks the
    /// placements (the clue map is genuinely the full grid).
    ///
    /// Carries no UA pre-filter ([`UaTier::Off`]) — the diagnostic walks use this so they
    /// pay nothing and measure the unfiltered baseline. Production strips use
    /// [`new_ua`](Self::new_ua).
    pub(in crate::generate) fn new(solution: &Solution) -> Self {
        Self::new_ua(solution, UaTier::Off)
    }

    /// [`new`](Self::new) carrying the `tier`'s UA pre-filter, built once from the full
    /// solution grid. The filter is sound (only fast-rejects prober reverts), so the strip
    /// trajectory is identical to [`UaTier::Off`] regardless of `tier`.
    pub(in crate::generate) fn new_ua(solution: &Solution, tier: UaTier) -> Self {
        let digits = solution.0.clone();
        debug_assert!(digits.is_complete(), "strip must start from a complete solution");
        let state = S::solved();
        debug_assert!(
            state == S::from_digits(&digits),
            "solved() must equal from_digits of a complete grid"
        );
        let clue = S::clue_map(&digits);
        let ua = UaFilter::build(&digits, tier);
        StripState { digits, state, clue, best: None, req_met: false, ua }
    }

    /// Revert a rejected strip of `cell` (held `orig`): restore the clue in both the
    /// placements shadow and the incremental candidate board (every carried view).
    fn revert(&mut self, cell: usize, orig: Digit) {
        self.digits.set(cell, orig);
        self.state.place_clue(&mut self.clue, cell, orig);
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
        let cand = self.state.clear_clue(&mut self.clue, cell, orig);
        debug_assert!(
            self.state == S::from_digits(&self.digits),
            "incremental strip-state drift after clear at {cell}"
        );
        cand.without(Mark::single(orig)).bits()
    }

    /// Fast-path keep of `cell` (see [`attempt`]): the cleared cell is still forced (a
    /// naked single, or a hidden single re-force), so both gates are skippable — commit the
    /// keep to the UA pre-filter and carry the running requirement verdict into `best`. The
    /// warp host's twin of the inline fast path.
    pub(in crate::generate) fn keep_trivial(&mut self, cell: usize) {
        self.ua.kept(cell);
        if self.req_met {
            self.best = Some(self.digits.clone());
        }
    }

    /// The UA pre-filter verdict for stripping `cell` (held `orig`): if some library UA
    /// would be emptied, the gate is a provable non-unique — revert it and return `true`,
    /// so the caller skips the (now-redundant) uniqueness probe. Returns `false` when no UA
    /// is emptied (pose the probe as usual). The cross-check (debug builds only) confirms
    /// the prober agrees with every catch; production builds skip the probe entirely.
    pub(in crate::generate) fn ua_revert_if_caught(&mut self, cell: usize, orig: Digit) -> bool {
        if self.ua.caught(cell) {
            debug_assert!(
                self.scalar_nonunique(cell, orig),
                "UA pre-filter false positive: prober finds cell {cell} unique"
            );
            self.revert(cell, orig);
            true
        } else {
            false
        }
    }

    /// The row-view candidate bands + empty mask as the packed prober's SoA input
    /// ([`crate::probe::simt::Probe`]'s `r`/`unsolved`) — bb's `export_r` twin: per digit
    /// its three 27-bit row-major bands ([`Bands::to_lanes`]), plus the still-empty mask.
    /// The carried `state` is read directly — no per-gate rebuild.
    /// The digit currently at `cell` in the placements shadow, or `None` if it has been
    /// stripped — the warp host's strip-walk reads it to skip already-removed cells (the
    /// `digits.get` of [`attempt`]'s loop).
    pub(in crate::generate) fn digit_at(&self, cell: usize) -> Option<Digit> {
        self.digits.get(cell)
    }

    pub(in crate::generate) fn export_r(&self) -> ([[u32; 4]; 9], [u32; 4]) {
        let row = self.state.row();
        let cand = row.candidates();
        (core::array::from_fn(|e| cand.each()[e].to_lanes()), row.unsolved().to_lanes())
    }

    /// After a speculative strip of `cell` (held `orig`) that left alternates
    /// (`alts != 0`): the units in which `orig` is still a **hidden single** for `cell`
    /// — bit 0 row, bit 1 column, bit 2 box; `0` = not re-forced. A non-zero mask
    /// proves the strip is trivially keepable, because every completion must put
    /// `orig` back at `cell` (the digit has no other candidate home in that unit), so
    /// the board's completions — and, one closure fixpoint later, its baseline trace —
    /// are exactly the accepted board's. Both gates are then skippable like the
    /// `alts == 0` fast path, and the trajectory is invariant (byte-identical
    /// puzzles); ~23% of gates / ~43% of accepts hit, worth ~5-7% e2e
    /// (`examples/reforcestat` tallies, `examples/reforceab` is the A/B). Reads
    /// everything off the row view: row and box are in-lane (one AND+popcount each);
    /// the column is its nine bits at offset `c` of each 9-bit row run across the
    /// three bands, summed — no column view needed, so the single-view strip state
    /// pays nothing extra for this test.
    pub(in crate::generate) fn reforced(&self, cell: usize, orig: Digit) -> u8 {
        let row = self.state.row();
        // Candidates are gated by `unsolved` (decided cells carry stale bits).
        let r = (row.candidates()[orig] & row.unsolved()).to_lanes();
        let row_word = r[cell / 27];
        // In-band 9-bit run of the cell's row; 3-bit-per-row block of its box.
        let row_mask = 0x1FFu32 << (9 * ((cell / 9) % 3));
        let box_mask = 0b111_000_000_111_000_000_111u32 << (3 * ((cell % 9) / 3));
        // Column `cc` occupies bit `cc` of each of the three 9-bit row runs, per band.
        let cc = cell % 9;
        let mut col_count = 0u32;
        for w in [r[0] >> cc, r[1] >> cc, r[2] >> cc] {
            col_count += (w & 1) + ((w >> 9) & 1) + ((w >> 18) & 1);
        }
        // `orig` is among `cell`'s candidates after the clear, so a count of 1
        // means the lone home IS `cell` — the strip is re-forced in that unit.
        let mut m = 0u8;
        m |= ((row_word & row_mask).count_ones() == 1) as u8;
        m |= ((col_count == 1) as u8) << 1;
        m |= (((row_word & box_mask).count_ones() == 1) as u8) << 2;
        m
    }

    /// The scalar uniqueness verdict for stripping `cell` (held `orig`): forbid `orig` on a
    /// clone of the row view and ask the existence prober whether an *alternate* completion
    /// exists (`true` = the strip is non-unique). [`attempt`] runs this inline at each gate;
    /// it is factored out so the packed-prober corpus (`collect_probes`) can record the exact
    /// reference verdict the SIMT `resolve_probes` must reproduce.
    pub(in crate::generate) fn scalar_nonunique(&self, cell: usize, orig: Digit) -> bool {
        let mut probe = self.state.row().clone();
        probe.forbid(cell, orig);
        P::has_completion(probe)
    }

    /// Revert a rejected gate (the prober found the strip non-unique) — the public twin
    /// of [`revert`](Self::revert) for the SIMT host, which decides the uniqueness verdict
    /// out-of-line (in the packed prober) and applies it here. Splits the uniqueness half
    /// of [`resolve_gate`](Self::resolve_gate) out so the baseline half can be deferred and
    /// batched onto the packed baseline solver.
    pub(in crate::generate) fn revert_gate(&mut self, cell: usize, orig: Digit) {
        self.revert(cell, orig);
    }

    /// Apply an already-computed baseline [`SolveTrace`] for a kept (unique) gate — the
    /// baseline half of [`resolve_gate`](Self::resolve_gate), split out so the SIMT host
    /// can run the baseline solve on the packed W=8 solver and feed the verdict back here.
    /// If the toolbox can't solve the strip, revert; else record the requirement verdict
    /// and snapshot `best`. The `trace` MUST be the verdict the scalar
    /// [`FusedLogicSolver`]/[`LogicSolver`] would return on this board (the packed solver
    /// is pinned byte-identical to it), so the outcome is unchanged by the deferral.
    pub(in crate::generate) fn apply_baseline(
        &mut self,
        cell: usize,
        orig: Digit,
        trace: &SolveTrace,
        spec: &Spec,
    ) {
        if !trace.solved {
            self.revert(cell, orig);
            return;
        }
        self.ua.kept(cell);
        self.req_met = spec.requirement_met(&trace.counts);
        if self.req_met {
            self.best = Some(self.digits.clone());
        }
    }
}

/// The scalar gate resolution — only on the dual-banded instantiation, because the
/// scalar baseline engines ([`FusedLogicSolver`]/[`LogicSolver`]) read both views.
/// The SIMT host never calls this (its baseline trace comes from the warp), which is
/// exactly why its strip can drop the column view.
impl StripState<DualSolverState> {
    /// Resolve the gates for the strip of `cell` (held `orig`) given the already-decided
    /// uniqueness verdict `nonunique`: if non-unique, revert; else run the baseline gate
    /// and either accept (updating `req_met`/`best`) or revert. `baseline` is the toolbox
    /// mask and `fast` selects the fused fast path vs the exact engine (see
    /// [`baseline_fast_applicable`]). The baseline reads the incrementally-maintained
    /// dual state directly — no per-gate rebuild — and the solver clones it internally,
    /// so the carried strip state is untouched.
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
        // The baseline view is the incrementally-maintained dual state — no rebuild.
        let trace = if fast {
            FusedLogicSolver::solve_tracked(&self.state, baseline)
        } else {
            LogicSolver::solve_tracked(&self.state, baseline)
        };
        if !trace.solved {
            self.revert(cell, orig);
            return;
        }
        self.ua.kept(cell);
        self.req_met = spec.requirement_met(&trace.counts);
        if self.req_met {
            self.best = Some(self.digits.clone());
        }
    }
}

/// One full strip attempt for `spec` at UA pre-filter [`tier`](UaTier). Mirrors the old bb
/// `generator`'s strip-attempt gate for gate, on the new prober/solver stack. The public
/// [`run_attempts`] / [`generate`] use [`UaTier::SCALAR`]; the tier is a parameter so a
/// bench/test can A/B it (the produced puzzles are tier-independent — the filter is sound).
pub fn attempt(rng: &mut Rng, spec: &Spec, tier: UaTier) -> AttemptResult {
    let baseline = spec.baseline_mask();
    let fast = baseline_fast_applicable(spec);
    let solution = random_solution(rng);
    // Strip order — the 81 cell indices shuffled; a fixed stack array, no per-attempt
    // heap alloc. Same shuffle as bb, so the RNG stream and produced puzzle match.
    let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
    rng.shuffle(&mut positions);

    // The scalar attempt strips on the dual-banded state (the default `StripView`):
    // its baseline gate below runs the scalar engines, which read both views.
    let mut st: StripState = StripState::new_ua(&solution, tier);
    for cell in positions {
        let Some(orig) = st.digit_at(cell) else {
            continue;
        };
        let alts = st.strip(cell, orig);
        // `alts == 0` fast path: the cleared cell is still a naked single, so its peers
        // already force `orig` — the strip stays unique AND baseline-solvable and the
        // requirement verdict is unchanged. Skip both gates; just carry `req_met`.
        if alts == 0 {
            st.keep_trivial(cell);
            continue;
        }
        // Hidden-single re-force fast path (see [`StripState::reforced`]): `orig` has no
        // other candidate home in one of `cell`'s units, so every completion re-forces it
        // — uniqueness and the baseline trace are exactly the accepted board's. Skip both
        // gates and carry the verdict, like `alts == 0`. Gated on the fused fast path
        // (a forced cheap kind reads exact cheap counts, which the skipped re-derivation
        // would shift by one hidden single).
        if fast && st.reforced(cell, orig) != 0 {
            st.keep_trivial(cell);
            continue;
        }
        // UA pre-filter (docs/UA-FILTER.md): if stripping `cell` would empty a library
        // unavoidable set, the gate is a provable non-unique — revert without posing the
        // probe (a probe deleted, not made cheaper). No-op for `UaTier::Off`.
        if st.ua_revert_if_caught(cell, orig) {
            continue;
        }
        // Uniqueness gate: forbid `orig` to restrict the cell to its alternates and ask
        // a single existence query (bb's `any_alt_solves`).
        let nonunique = st.scalar_nonunique(cell, orig);
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
        match attempt(rng, spec, UaTier::SCALAR) {
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
/// fps are directly comparable. Uses the production UA pre-filter tier ([`UaTier::SCALAR`]).
pub fn run_attempts(rng: &mut Rng, spec: &Spec, n: usize) -> (Stats, u64) {
    run_attempts_ua(rng, spec, n, UaTier::SCALAR)
}

/// [`run_attempts`] at an explicit UA pre-filter [`tier`](UaTier) — the A/B entry the
/// filter-on-vs-off fingerprint test and the scalar bench drive. The fingerprint is
/// tier-independent (the filter is sound: it never changes a verdict), which that test pins.
pub fn run_attempts_ua(rng: &mut Rng, spec: &Spec, n: usize, tier: UaTier) -> (Stats, u64) {
    let mut stats = Stats::default();
    let mut fp: u64 = FNV_OFFSET;
    for _ in 0..n {
        stats.attempts += 1;
        match attempt(rng, spec, tier) {
            AttemptResult::Success(p) => {
                stats.successes += 1;
                stats.total_givens += p.givens;
                fnv_fold_cells(&mut fp, &p.puzzle.0.cell_bytes());
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

/// Tallies from a [`reforce_stat`] walk — the diagnostic for the hidden-single
/// re-force fast path (see [`StripState::reforced`]). The three `hit_*`
/// violation counters check the soundness claims empirically and must all be zero:
/// a hit gate must be unique, baseline-solvable, and leave the requirement verdict
/// exactly as carried (the values the fast path would skip computing).
#[derive(Default, Clone, Copy, Debug)]
pub struct ReforceStat {
    pub attempts: usize,
    /// `alts == 0` fast-path keeps (the existing trivial path).
    pub trivial: usize,
    /// `alts != 0` gates (probe + maybe baseline today).
    pub gates: usize,
    pub accepted: usize,
    pub rejected_nonunique: usize,
    pub rejected_unsolvable: usize,
    /// Gates where [`StripState::reforced`] fired (any unit) — each would skip both gates.
    pub hits: usize,
    pub hit_row: usize,
    pub hit_col: usize,
    pub hit_box: usize,
    // Soundness violations — must be zero.
    pub hit_nonunique: usize,
    pub hit_unsolvable: usize,
    pub hit_req_drift: usize,
}

/// Walk `attempts` faithful scalar strip attempts from `base_seed` and tally, at every
/// `alts != 0` gate, whether the hidden-single re-force test fires and whether its
/// soundness claims hold against the real gate verdicts (which are computed and applied
/// as in production, so the walk's trajectory is exactly [`attempt`]'s — every gate is
/// evaluated here even where production now skips it; that is the point). Diagnostic
/// only; the production fast path lives in [`attempt`] / the warp's `step_to_gate`.
pub fn reforce_stat(base_seed: u64, spec: &Spec, attempts: usize) -> ReforceStat {
    let baseline = spec.baseline_mask();
    let fast = baseline_fast_applicable(spec);
    let mut rng = Rng::from_seed(base_seed);
    let mut s = ReforceStat::default();
    for _ in 0..attempts {
        s.attempts += 1;
        let solution = random_solution(&mut rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        let mut st: StripState = StripState::new(&solution);
        for cell in positions {
            let Some(orig) = st.digit_at(cell) else {
                continue;
            };
            let alts = st.strip(cell, orig);
            if alts == 0 {
                st.keep_trivial(cell);
                s.trivial += 1;
                continue;
            }
            s.gates += 1;
            let rf = st.reforced(cell, orig);
            if rf != 0 {
                s.hits += 1;
                s.hit_row += (rf & 1 != 0) as usize;
                s.hit_col += (rf & 2 != 0) as usize;
                s.hit_box += (rf & 4 != 0) as usize;
            }
            // The real gates, inlined from `resolve_gate` so the trace is visible.
            let nonunique = st.scalar_nonunique(cell, orig);
            if nonunique {
                s.rejected_nonunique += 1;
                s.hit_nonunique += (rf != 0) as usize;
                st.revert_gate(cell, orig);
                continue;
            }
            let trace = if fast {
                FusedLogicSolver::solve_tracked(&st.state, baseline)
            } else {
                LogicSolver::solve_tracked(&st.state, baseline)
            };
            if !trace.solved {
                s.rejected_unsolvable += 1;
                s.hit_unsolvable += (rf != 0) as usize;
                st.revert_gate(cell, orig);
                continue;
            }
            s.accepted += 1;
            let req = spec.requirement_met(&trace.counts);
            if rf != 0 && req != st.req_met {
                s.hit_req_drift += 1;
            }
            st.req_met = req;
            if req {
                st.best = Some(st.digits.clone());
            }
        }
    }
    s
}

/// Per-clue-count + run-shape tallies from a [`defer_stat`] walk — the deferred-strip
/// (M1) and common-snapshot (M3) measurement. Every array is indexed by **clue count**
/// (board fullness = givens still on the board at the gate, `0..=81`), the position axis
/// the deferred analysis bins on, NOT the loop index. Posed probes split keep (the prober
/// returned *unique*) vs revert (*non-unique*), with the prober DFS node count
/// ([`crate::probe`] `PCTR[2]`) summed per disposition; the two fast paths (`alts == 0`,
/// re-force) are tallied separately as skips. `run_hist[k]` counts maximal runs of `k`
/// consecutive keeps among the strip steps, where a fast-path skip counts as a keep (a
/// deferred batch would absorb it) and only a *non-unique* posed gate breaks a run — the
/// k-distribution deferred stripping would exploit. The `*_passes`/`*_gates` sums are the
/// M3 common-snapshot sizing: per kept gate, the prober's root drain (propagation before
/// the first branch) vs its total propagation, and the baseline solve's initial
/// closure-drain to first fixpoint vs its total — both measured by replaying the drain
/// standalone on the just-stripped board and reading the relevant counter.
#[cfg(feature = "count")]
#[derive(Clone)]
pub struct DeferStat {
    pub attempts: usize,
    /// Posed uniqueness probes (after both fast paths missed), by clue count.
    pub posed: [u64; 82],
    /// Fast-path skips (`alts == 0` or re-force), by clue count.
    pub fastpath: [u64; 82],
    /// Posed probes whose verdict was unique (keep), by clue count.
    pub keeps: [u64; 82],
    /// Posed probes whose verdict was non-unique (revert), by clue count.
    pub reverts: [u64; 82],
    /// Prober DFS nodes summed over unique (keep) probes, by clue count.
    pub nodes_keep: [u64; 82],
    /// Prober DFS nodes summed over non-unique (revert) probes, by clue count.
    pub nodes_revert: [u64; 82],
    /// Unique probes the baseline gate then reverted as unsolvable (a subset of
    /// `keeps`; the uniqueness verdict was still a keep) — reported so the run
    /// histogram's keep-by-uniqueness definition can be re-cut if desired.
    pub baseline_revert: [u64; 82],
    /// `run_hist[k]` = number of maximal consecutive-keep runs of length `k`
    /// (`k` in `1..=81`; fast-path skips and unique probes are keeps, non-unique
    /// probes break a run).
    pub run_hist: [u64; 82],
    /// UA-witness size histogram for the probe-skip idea: at each non-unique (revert)
    /// gate, the cell-diff between the known solution and the alternate completion the
    /// prober constructed — itself an emptied unavoidable set, so its popcount is the
    /// witness size. Indexed `[clue group][size]`, group 0 = clue >= 33, group 1 =
    /// clue <= 32; `size` in `0..=81` (UAs are even and >= 4). The cumulative tail
    /// `<= k` is a lower bound on a complete size-`k` UA library's revert catch-rate.
    pub witness_size: [[u64; 82]; 2],
    /// The same reverts as [`witness_size`](Self::witness_size) but each weighted by the
    /// revert probe's DFS node count — so the cumulative tail `<= k` is the *cost* a
    /// size-`k` library reclaims, not just the count fraction. Small-UA reverts cluster at
    /// high clue where probes are ~1 node, so the node-weighted catch runs well below the
    /// count-weighted one — the number the e2e economics actually turn on.
    pub witness_nodes: [[u64; 82]; 2],
    /// M3 prober root-drain band-passes summed over keep (unique) gates.
    pub probe_root_passes: u64,
    /// M3 prober total band-passes summed over the same gates.
    pub probe_total_passes: u64,
    /// Number of keep gates the prober drain was measured on.
    pub probe_drain_gates: u64,
    /// M3 baseline closure-drain sieve-recomputes (drain to first fixpoint) summed
    /// over accepted (unique + baseline-solvable) gates.
    pub solve_drain_passes: u64,
    /// M3 baseline total sieve-recomputes summed over the same gates.
    pub solve_total_passes: u64,
    /// Number of accepted gates the baseline drain was measured on.
    pub solve_drain_gates: u64,
}

/// Walk `attempts` faithful scalar strip attempts from `base_seed` and record the
/// deferred-strip (M1) and common-snapshot (M3) instrumentation — the trajectory is
/// exactly [`attempt`]'s (every gate is evaluated and applied as in production), so the
/// per-gate statistics transfer to the warp rig it is pinned to. Behind `feature =
/// "count"`: it snapshots the prober node counter (`PCTR`), the prober band-pass counter
/// (`BAND_CTR`), and the baseline sieve-recompute counter (`FSTAT`) per gate, so it must
/// not be mixed with a wall-time A/B (the counters perturb timing). Diagnostic only.
#[cfg(feature = "count")]
pub fn defer_stat(base_seed: u64, spec: &Spec, attempts: usize) -> DeferStat {
    use crate::probe::search::first_completion;
    use crate::probe::{
        Propagate, band_ctr_reset, band_ctr_snapshot, pctr_reset, pctr_snapshot,
    };
    use crate::solve::{fstat_reset, fstat_snapshot};

    let baseline = spec.baseline_mask();
    let fast = baseline_fast_applicable(spec);
    assert!(fast, "defer_stat assumes the fused fast-path baseline (none of the bench specs need LogicSolver)");
    // The cheap closure (singles + both LC) — solving with only this mask drains the
    // board to its first fixpoint and then stalls (no harder kind is allowed), so its
    // sieve-recompute count IS the baseline's initial closure-drain cost.
    const NS: KindMask = 1 << NAKED_SINGLE;
    const HS: KindMask = 1 << HIDDEN_SINGLE;
    const LCP: KindMask = 1 << LC_POINTING;
    const LCC: KindMask = 1 << LC_CLAIMING;
    let cheap = baseline & (NS | HS | LCP | LCC);

    let mut rng = Rng::from_seed(base_seed);
    let mut s = DeferStat {
        attempts: 0,
        posed: [0; 82],
        fastpath: [0; 82],
        keeps: [0; 82],
        reverts: [0; 82],
        nodes_keep: [0; 82],
        nodes_revert: [0; 82],
        baseline_revert: [0; 82],
        run_hist: [0; 82],
        witness_size: [[0; 82]; 2],
        witness_nodes: [[0; 82]; 2],
        probe_root_passes: 0,
        probe_total_passes: 0,
        probe_drain_gates: 0,
        solve_drain_passes: 0,
        solve_total_passes: 0,
        solve_drain_gates: 0,
    };

    for _ in 0..attempts {
        s.attempts += 1;
        let solution = random_solution(&mut rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        let mut st: StripState = StripState::new(&solution);
        // Per-digit cells of the full solution (row-major) — the witness diff's reference.
        let sol_cells = SolverState::<RM>::clue_map(&solution.0);
        // Clue count at the current gate = givens on the board after the speculative
        // strip; starts at 81 (full grid), -1 per strip, +1 per revert.
        let mut givens = CELLS;
        // Length of the current maximal run of consecutive keeps.
        let mut run = 0usize;
        macro_rules! end_run {
            () => {
                if run > 0 {
                    s.run_hist[run.min(81)] += 1;
                    run = 0;
                }
            };
        }

        for cell in positions {
            let Some(orig) = st.digit_at(cell) else {
                continue;
            };
            let alts = st.strip(cell, orig);
            givens -= 1;
            let clue = givens;
            // Fast paths: the cell stays stripped with no gate posed — a keep that a
            // deferred batch absorbs.
            if alts == 0 || (fast && st.reforced(cell, orig) != 0) {
                st.keep_trivial(cell);
                s.fastpath[clue] += 1;
                run += 1;
                continue;
            }
            // Posed uniqueness probe — measure its nodes and total band-passes.
            s.posed[clue] += 1;
            pctr_reset();
            band_ctr_reset();
            let nonunique = st.scalar_nonunique(cell, orig);
            let nodes = pctr_snapshot()[2];
            let total_passes = band_ctr_snapshot()[1];
            if nonunique {
                // Non-unique: revert, and the run breaks here.
                s.reverts[clue] += 1;
                s.nodes_revert[clue] += nodes;
                // UA witness — reconstruct the alternate completion this revert proves
                // exists and diff it against the solution over the empty cells (givens
                // agree, so the diff lies wholly in the empties): the emptied UA, whose
                // popcount a size-capped library would have to cover to skip this probe.
                let mut probe = st.state.row().clone();
                probe.forbid(cell, orig);
                let empty = probe.unsolved();
                if let Some(alt) = first_completion::<RM, Bivalue>(probe) {
                    let mut diff = 0u32;
                    for d in 0..9 {
                        let m = (sol_cells.each()[d] & empty & !alt.candidates().each()[d]).to_lanes();
                        diff += m[0].count_ones() + m[1].count_ones() + m[2].count_ones();
                    }
                    let group = if clue >= 33 { 0 } else { 1 };
                    let bin = (diff as usize).min(81);
                    s.witness_size[group][bin] += 1;
                    s.witness_nodes[group][bin] += nodes;
                }
                st.revert_gate(cell, orig);
                givens += 1;
                end_run!();
                continue;
            }
            // Unique (keep by the prober's verdict) — continues the run.
            s.keeps[clue] += 1;
            s.nodes_keep[clue] += nodes;
            run += 1;

            // M3 part 1: the prober's root drain — replay the first propagation on a
            // clone of the just-stripped probe board (orig forbidden), the exact drain
            // the prober does before its first branch; band-passes vs the probe total.
            band_ctr_reset();
            let mut root = st.state.row().clone();
            root.forbid(cell, orig);
            <RM as Propagate>::propagate(&mut root);
            s.probe_root_passes += band_ctr_snapshot()[1];
            s.probe_total_passes += total_passes;
            s.probe_drain_gates += 1;

            // M3 part 2: the baseline solve's closure-drain to first fixpoint (cheap-only
            // solve) vs the full solve's total, in sieve-recomputes. The full solve is the
            // real baseline gate; its trace drives the keep/revert decision below.
            fstat_reset();
            let _ = FusedLogicSolver::solve_tracked(&st.state, cheap);
            let drain_passes = fstat_snapshot()[7];
            fstat_reset();
            let trace = FusedLogicSolver::solve_tracked(&st.state, baseline);
            let solve_total = fstat_snapshot()[7];
            s.solve_drain_passes += drain_passes;
            s.solve_total_passes += solve_total;
            s.solve_drain_gates += 1;

            // Apply the real baseline gate (faithful to resolve_gate's fast path).
            if !trace.solved {
                st.revert_gate(cell, orig);
                givens += 1;
                s.baseline_revert[clue] += 1;
            } else {
                st.req_met = spec.requirement_met(&trace.counts);
                if st.req_met {
                    st.best = Some(st.digits.clone());
                }
            }
        }
        // Trailing run (no reset needed — `run` is re-initialized next attempt).
        if run > 0 {
            s.run_hist[run.min(81)] += 1;
        }
    }
    s
}

/// A board's 2-digit unavoidable-set library: every minimal UA built from a single
/// digit pair, as a cell list, plus the cell -> containing-UA index. For a complete
/// grid and a pair `{a, b}`, a swap of `a <-> b` on a cell set is another valid grid iff
/// in EVERY unit the set holds the `a`-cell iff it holds the `b`-cell — so the minimal
/// such sets are the connected components of the graph that joins, per unit (row, column,
/// AND box), that unit's `a`-cell to its `b`-cell. Components have even size 4..=18; the
/// size-4 ones are the deadly rectangles (UA4s). This is the *exact* minimal library a
/// probe-skip filter would carry; unlike a prober witness it never overshoots.
#[cfg(feature = "count")]
struct UaLib {
    /// `(cells, size)` per UA.
    uas: Vec<(Vec<usize>, usize)>,
    /// `cell -> UA indices containing it` (length [`CELLS`]).
    cell_uas: Vec<Vec<usize>>,
}

/// Enumerate the 2-digit UA library of a complete solution (see [`UaLib`]). Union-find
/// over the cells of each of the 36 digit pairs, joining each unit's two pair-cells; the
/// resulting components are the minimal 2-digit UAs.
#[cfg(feature = "count")]
fn enumerate_2digit_uas(sol: &DigitGrid) -> UaLib {
    fn find(p: &mut [usize], x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        let mut c = x;
        while p[c] != r {
            let n = p[c];
            p[c] = r;
            c = n;
        }
        r
    }
    let digit = |cell: usize| sol.get(cell).map(|d| d.index());
    // The `k`-th cell of unit `u` of kind 0=row / 1=column / 2=box.
    let unit_cell = |kind: usize, u: usize, k: usize| -> usize {
        match kind {
            0 => u * 9 + k,
            1 => k * 9 + u,
            _ => ((u / 3) * 3 + k / 3) * 9 + ((u % 3) * 3 + k % 3),
        }
    };
    let mut uas: Vec<(Vec<usize>, usize)> = Vec::new();
    let mut cell_uas: Vec<Vec<usize>> = (0..CELLS).map(|_| Vec::new()).collect();
    for a in 0..9usize {
        for b in (a + 1)..9usize {
            let mut parent: Vec<usize> = (0..CELLS).collect();
            for kind in 0..3 {
                for u in 0..9 {
                    // Each unit holds exactly one `a`-cell and one `b`-cell; join them.
                    let mut found = [usize::MAX; 2];
                    let mut n = 0;
                    for k in 0..9 {
                        let cell = unit_cell(kind, u, k);
                        if matches!(digit(cell), Some(di) if di == a || di == b) {
                            if n < 2 {
                                found[n] = cell;
                            }
                            n += 1;
                        }
                    }
                    if n == 2 {
                        let (ra, rb) = (find(&mut parent, found[0]), find(&mut parent, found[1]));
                        if ra != rb {
                            parent[ra] = rb;
                        }
                    }
                }
            }
            // Group this pair's cells into components -> UAs.
            let mut roots: Vec<(usize, usize)> = Vec::new(); // (root, ua index)
            for cell in 0..CELLS {
                if !matches!(digit(cell), Some(di) if di == a || di == b) {
                    continue;
                }
                let r = find(&mut parent, cell);
                let idx = match roots.iter().find(|&&(rr, _)| rr == r) {
                    Some(&(_, i)) => i,
                    None => {
                        let i = uas.len();
                        uas.push((Vec::new(), 0));
                        roots.push((r, i));
                        i
                    }
                };
                uas[idx].0.push(cell);
                cell_uas[cell].push(idx);
            }
            for &(_, i) in &roots {
                uas[i].1 = uas[i].0.len();
            }
        }
    }
    UaLib { uas, cell_uas }
}

/// True 2-digit-UA probe-skip catch rate (M-UA-LIB) — the decisive counter to the
/// witness histogram, which over-credits large orbits. Per board, enumerate the exact
/// 2-digit UA library ([`enumerate_2digit_uas`]); along the faithful strip walk, keep a
/// per-UA given count (monotone, decremented on each committed keep). A revert at cell
/// `c` is *caught* by the library iff some library UA containing `c` is down to its last
/// given (`c`) — stripping it provably empties that UA. Records catch by count and by the
/// revert's prober node cost, for the UA4-only and the full 2-digit tier, split by clue
/// group; plus the locked-cell census (sole givens of a UA — permanently unstrippable)
/// per clue count. `false_positive` (a unique gate the filter wrongly flagged) is a
/// soundness tripwire and must stay zero.
#[cfg(feature = "count")]
#[derive(Clone)]
pub struct UaCatchStat {
    pub attempts: usize,
    pub boards: u64,
    pub lib_uas: u64,
    pub lib_ua4: u64,
    /// `[group]` 0 = clue >= 33, 1 = clue <= 32.
    pub reverts: [u64; 2],
    pub revert_nodes: [u64; 2],
    pub caught_ua4_ct: [u64; 2],
    pub caught_ua4_nd: [u64; 2],
    pub caught_all_ct: [u64; 2],
    pub caught_all_nd: [u64; 2],
    /// Locked-cell census by clue count: sum and sample-count of the locked-cell tally
    /// (sole givens of some library UA) observed at each clue level.
    pub locked_sum: [u64; 82],
    pub locked_n: [u64; 82],
    /// Soundness tripwire: a unique (kept or baseline-reverted) gate the filter flagged.
    pub false_positive: u64,
}

#[cfg(feature = "count")]
pub fn ua_catch_stat(base_seed: u64, spec: &Spec, attempts: usize) -> UaCatchStat {
    use crate::probe::{pctr_reset, pctr_snapshot};

    let baseline = spec.baseline_mask();
    let fast = baseline_fast_applicable(spec);
    assert!(fast, "ua_catch_stat assumes the fused fast-path baseline");
    let mut rng = Rng::from_seed(base_seed);
    let mut s = UaCatchStat {
        attempts: 0,
        boards: 0,
        lib_uas: 0,
        lib_ua4: 0,
        reverts: [0; 2],
        revert_nodes: [0; 2],
        caught_ua4_ct: [0; 2],
        caught_ua4_nd: [0; 2],
        caught_all_ct: [0; 2],
        caught_all_nd: [0; 2],
        locked_sum: [0; 82],
        locked_n: [0; 82],
        false_positive: 0,
    };

    for _ in 0..attempts {
        s.attempts += 1;
        let solution = random_solution(&mut rng);
        let lib = enumerate_2digit_uas(&solution.0);
        s.boards += 1;
        s.lib_uas += lib.uas.len() as u64;
        s.lib_ua4 += lib.uas.iter().filter(|u| u.1 == 4).count() as u64;

        // Per-UA given count (full board: every cell a given), and the locked census.
        let mut gc: Vec<u32> = lib.uas.iter().map(|u| u.1 as u32).collect();
        let mut is_given = [true; CELLS];
        let mut locked = [false; CELLS];
        let mut n_locked = 0u64;

        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        let mut st: StripState = StripState::new(&solution);
        let mut givens = CELLS;

        for cell in positions {
            let Some(orig) = st.digit_at(cell) else {
                continue;
            };
            // Census at this gate (board still has `givens` clues).
            s.locked_sum[givens.min(81)] += n_locked;
            s.locked_n[givens.min(81)] += 1;
            // Filter verdict: would stripping `cell` empty a library UA? (`cell` is still
            // a committed given here, so a count of 1 means it is that UA's sole given.)
            let mut caught_ua4 = false;
            let mut caught_all = false;
            for &u in &lib.cell_uas[cell] {
                if gc[u] == 1 {
                    caught_all = true;
                    caught_ua4 |= lib.uas[u].1 == 4;
                }
            }

            let alts = st.strip(cell, orig);
            givens -= 1;
            let clue = givens;
            let group = if clue >= 33 { 0 } else { 1 };
            let mut nonuniq = false;
            let mut keep = false;
            if alts == 0 || (fast && st.reforced(cell, orig) != 0) {
                st.keep_trivial(cell);
                keep = true;
            } else {
                pctr_reset();
                let nonunique = st.scalar_nonunique(cell, orig);
                let nodes = pctr_snapshot()[2];
                if nonunique {
                    nonuniq = true;
                    s.reverts[group] += 1;
                    s.revert_nodes[group] += nodes;
                    if caught_ua4 {
                        s.caught_ua4_ct[group] += 1;
                        s.caught_ua4_nd[group] += nodes;
                    }
                    if caught_all {
                        s.caught_all_ct[group] += 1;
                        s.caught_all_nd[group] += nodes;
                    }
                    st.revert_gate(cell, orig);
                    givens += 1;
                } else {
                    let trace = FusedLogicSolver::solve_tracked(&st.state, baseline);
                    if !trace.solved {
                        st.revert_gate(cell, orig);
                        givens += 1;
                    } else {
                        st.req_met = spec.requirement_met(&trace.counts);
                        if st.req_met {
                            st.best = Some(st.digits.clone());
                        }
                        keep = true;
                    }
                }
            }
            // Soundness: a unique gate (fast keep, kept, or baseline-revert) emptied no UA.
            if !nonuniq && caught_all {
                s.false_positive += 1;
            }
            // Commit a keep: decrement the UA given counts, and lock any UA's last given.
            if keep {
                is_given[cell] = false;
                for &u in &lib.cell_uas[cell] {
                    gc[u] -= 1;
                    if gc[u] == 1 {
                        for &cc in &lib.uas[u].0 {
                            if is_given[cc] {
                                if !locked[cc] {
                                    locked[cc] = true;
                                    n_locked += 1;
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    s
}

/// Wall-time split of one fixed-work scalar run between the cold [`verify`] (the
/// `min_target_uses` avoid walk) and the rest of the attempt — the M2 measurement. NO
/// counters: timing only, so it must be built WITHOUT `feature = "count"` (counters
/// perturb the very times this measures). Times are nanoseconds; `total_nanos` is the
/// whole run (fill + strip + verify), `verify_nanos` the verify subset.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default, Clone, Copy, Debug)]
pub struct VerifyShare {
    pub attempts: usize,
    /// Attempts whose strip produced a requirement-meeting `best` (so verify ran).
    pub reached_verify: usize,
    /// Of those, verify rejected (the target was substitutable) — `NotForced`.
    pub not_forced: usize,
    pub successes: usize,
    pub total_nanos: u64,
    pub verify_nanos: u64,
    /// Posed probes whose verdict was unique (keep), and the wall time in them.
    pub keep_probes: usize,
    pub keep_probe_nanos: u64,
    /// Posed probes whose verdict was non-unique (revert), and the wall time in them —
    /// the revert-side prober cost a probe-skip library would reclaim. The per-probe
    /// `Instant` adds a small (~75 ns/probe) overhead; the keep/revert wall-time split it
    /// yields cross-checks the (clean) revert-node share, and the verify share it shares
    /// the walk with is a ratio, so the drift cancels.
    pub revert_probes: usize,
    pub revert_probe_nanos: u64,
}

/// Walk `attempts` faithful scalar strip attempts from `base_seed`, timing the cold
/// [`verify`] call against the whole attempt. The strip loop is exactly [`attempt`]'s
/// (same gate sequence, same produced `best`), so the (successes, not_forced,
/// never_fired) split matches [`run_attempts`] for the seed — only the timing split is
/// added. Diagnostic only; keep it out of `feature = "count"` builds.
#[cfg(not(target_arch = "wasm32"))]
pub fn verify_share(base_seed: u64, spec: &Spec, attempts: usize) -> VerifyShare {
    use std::time::Instant;

    let baseline = spec.baseline_mask();
    let fast = baseline_fast_applicable(spec);
    let mut rng = Rng::from_seed(base_seed);
    let mut vs = VerifyShare::default();

    for _ in 0..attempts {
        vs.attempts += 1;
        let t_att = Instant::now();
        let solution = random_solution(&mut rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        let mut st: StripState = StripState::new(&solution);
        for cell in positions {
            let Some(orig) = st.digit_at(cell) else {
                continue;
            };
            let alts = st.strip(cell, orig);
            if alts == 0 || (fast && st.reforced(cell, orig) != 0) {
                st.keep_trivial(cell);
                continue;
            }
            let tp = Instant::now();
            let nonunique = st.scalar_nonunique(cell, orig);
            let pe = tp.elapsed().as_nanos() as u64;
            if nonunique {
                vs.revert_probes += 1;
                vs.revert_probe_nanos += pe;
            } else {
                vs.keep_probes += 1;
                vs.keep_probe_nanos += pe;
            }
            st.resolve_gate(cell, orig, nonunique, spec, baseline, fast);
        }
        if let Some(snap) = st.best.take() {
            vs.reached_verify += 1;
            let tv = Instant::now();
            let ok = verify(&snap, spec);
            vs.verify_nanos += tv.elapsed().as_nanos() as u64;
            if ok {
                vs.successes += 1;
            } else {
                vs.not_forced += 1;
            }
        }
        vs.total_nanos += t_att.elapsed().as_nanos() as u64;
    }
    vs
}

/// Wall time to build the UA pre-filter `tier`'s library over `attempts` random solutions
/// from `base_seed` — the per-board enumeration cost in isolation (the stage-2 decision
/// gate: on SIMT the full library only beats UA4-only if its build stays ~<= 1 us/board).
/// Solutions are filled up front so only the enumeration is timed; returns `(total_nanos,
/// total_uas)` for ns/board and avg library size. Diagnostic only (no `count` feature).
#[cfg(not(target_arch = "wasm32"))]
pub fn ua_build_cost(base_seed: u64, attempts: usize, tier: UaTier) -> (u64, u64) {
    use std::time::Instant;
    let mut rng = Rng::from_seed(base_seed);
    let sols: Vec<DigitGrid> = (0..attempts).map(|_| random_solution(&mut rng).0).collect();
    let mut uas = 0u64;
    let t = Instant::now();
    for s in &sols {
        // `uas` accumulates the library size so the build cannot be optimized away.
        uas += UaFilter::build(s, tier).nua as u64;
    }
    (t.elapsed().as_nanos() as u64, uas)
}

/// Cross-backend determinism fingerprint over `n` attempts' worth of the RNG stream.
/// Unlike [`run_attempts`]'s fp (which only folds *successful* puzzles, so it is blind to
/// the grids when nothing succeeds), this folds the full solution grid AND the shuffled
/// strip order of every iteration — the two and only two RNG consumers of an attempt (the
/// prober and baseline take no RNG). Native and wasm32 MUST return the same value; that is
/// the guard that the Lemire `range`/shuffle and the fill are target-independent. It walks
/// the identical RNG trajectory as `n` attempts (the strip consumes no RNG), so it is a
/// faithful probe. This is a correctness guard, not a perf metric. The digit fold via
/// [`DigitGrid::cell_bytes`] is pinned per seed in `tests/faithful` (and cross-checked by
/// the wasm `det_fp` export).
pub fn determinism_fp(rng: &mut Rng, n: usize) -> u64 {
    let mut fp: u64 = FNV_OFFSET;
    for _ in 0..n {
        let cells = random_solution(rng).0.cell_bytes();
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

#[cfg(test)]
mod ua_filter_tests {
    use super::{CELLS, UaFilter, UaTier};
    use crate::fill::random_solution;
    use crate::repr::{Digit, DigitGrid};
    use crate::rng::Rng;

    /// Reconstruct each UA's sorted cell list from the filter's `cell -> UA` index, and
    /// cross-check the per-UA `counts` (the full-board given count = `|U|`).
    fn cellsets(f: &UaFilter) -> Vec<Vec<usize>> {
        let mut sets: Vec<Vec<usize>> = vec![Vec::new(); f.nua];
        for cell in 0..CELLS {
            for &id in &f.cell_uas[cell][..f.lens[cell] as usize] {
                sets[id as usize].push(cell);
            }
        }
        for (id, s) in sets.iter_mut().enumerate() {
            s.sort_unstable();
            assert_eq!(f.counts[id] as usize, s.len(), "UA {id} count != membership");
        }
        sets
    }

    fn sorted(mut v: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
        v.sort();
        v
    }

    fn is_valid_complete(g: &DigitGrid) -> bool {
        let unit_cell = |kind: usize, u: usize, k: usize| -> usize {
            match kind {
                0 => u * 9 + k,
                1 => k * 9 + u,
                _ => ((u / 3) * 3 + k / 3) * 9 + ((u % 3) * 3 + k % 3),
            }
        };
        for kind in 0..3 {
            for u in 0..9 {
                let mut seen = [false; 9];
                for k in 0..9 {
                    let Some(d) = g.get(unit_cell(kind, u, k)) else {
                        return false;
                    };
                    if seen[d.index()] {
                        return false;
                    }
                    seen[d.index()] = true;
                }
            }
        }
        true
    }

    /// The cheap UA4 rectangle enumeration must yield EXACTLY the size-4 components of the
    /// full 2-digit union-find — otherwise the UA4-only tier would miss (or invent) deadly
    /// rectangles, losing catch. (Soundness is independent: the filter only ever reverts.)
    #[test]
    fn ua4_equals_full_size4() {
        for seed in 0..50 {
            let mut rng = Rng::from_seed(seed);
            let sol = random_solution(&mut rng).0;
            let ua4 = sorted(cellsets(&UaFilter::build(&sol, UaTier::Ua4)));
            let full = UaFilter::build(&sol, UaTier::Full);
            let full_s4 = sorted(cellsets(&full).into_iter().filter(|s| s.len() == 4).collect());
            assert!(ua4.iter().all(|s| s.len() == 4), "seed {seed}: non-size-4 UA in UA4 tier");
            assert_eq!(ua4, full_s4, "seed {seed}: UA4 rectangle set != full size-4 components");
        }
    }

    /// Every UA the full enumeration produces is a genuine unavoidable set: it spans 2
    /// digits, has even size >= 4, and swapping those digits over its cells yields a
    /// *different, still valid* complete grid. The non-count twin of
    /// `ua_tests::enumerated_uas_are_genuine_unavoidable_sets` for the production
    /// [`UaFilter::enumerate_2digit`] port.
    #[test]
    fn full_uas_are_genuine() {
        for seed in 0..30 {
            let mut rng = Rng::from_seed(seed);
            let sol = random_solution(&mut rng).0;
            let f = UaFilter::build(&sol, UaTier::Full);
            assert!(f.nua > 0, "seed {seed}: empty full library");
            for cells in cellsets(&f) {
                assert!(cells.len() >= 4 && cells.len() % 2 == 0, "seed {seed}: bad UA size");
                let mut digs: Vec<usize> = cells.iter().map(|&c| sol.get(c).unwrap().index()).collect();
                digs.sort_unstable();
                digs.dedup();
                assert_eq!(digs.len(), 2, "seed {seed}: UA spans {} digits", digs.len());
                let (a, b) = (digs[0], digs[1]);
                let mut swapped = sol.clone();
                for &c in &cells {
                    let nd = if sol.get(c).unwrap().index() == a { b } else { a };
                    swapped.set(c, Digit::from_index(nd));
                }
                assert!(is_valid_complete(&swapped), "seed {seed}: UA swap is not a valid grid");
                assert_ne!(swapped.to_line(), sol.to_line(), "seed {seed}: UA swap is the identity");
            }
        }
    }

    /// Sanity anchors from M-UA-LIB (seed-1 fill distribution): ~10.7 UA4s/board and ~55
    /// total 2-digit UAs/board, well under the [`super::UA_CAP`] of 192 (no truncation).
    #[test]
    fn library_sizes_match_anchors() {
        let mut rng = Rng::from_seed(1);
        let (mut ua4, mut full, mut max_nua) = (0usize, 0usize, 0usize);
        let n = 400;
        for _ in 0..n {
            let sol = random_solution(&mut rng).0;
            ua4 += UaFilter::build(&sol, UaTier::Ua4).nua;
            let f = UaFilter::build(&sol, UaTier::Full);
            full += f.nua;
            max_nua = max_nua.max(f.nua);
        }
        let (a4, af) = (ua4 as f64 / n as f64, full as f64 / n as f64);
        assert!((9.0..12.5).contains(&a4), "UA4/board {a4} off the ~10.7 anchor");
        assert!((50.0..60.0).contains(&af), "UA/board {af} off the ~55 anchor");
        assert!(max_nua < super::UA_CAP, "max UAs/board {max_nua} >= UA_CAP (would truncate)");
    }
}

#[cfg(all(test, feature = "count"))]
mod ua_tests {
    use super::enumerate_2digit_uas;
    use crate::fill::random_solution;
    use crate::repr::{Digit, DigitGrid};
    use crate::rng::Rng;

    fn is_valid_complete(g: &DigitGrid) -> bool {
        let unit_cell = |kind: usize, u: usize, k: usize| -> usize {
            match kind {
                0 => u * 9 + k,
                1 => k * 9 + u,
                _ => ((u / 3) * 3 + k / 3) * 9 + ((u % 3) * 3 + k % 3),
            }
        };
        for kind in 0..3 {
            for u in 0..9 {
                let mut seen = [false; 9];
                for k in 0..9 {
                    let Some(d) = g.get(unit_cell(kind, u, k)) else {
                        return false;
                    };
                    if seen[d.index()] {
                        return false;
                    }
                    seen[d.index()] = true;
                }
            }
        }
        true
    }

    /// Every enumerated 2-digit UA must be a genuine unavoidable set: it spans exactly two
    /// digits, has even size >= 4, and swapping those two digits over its cells yields a
    /// *different, still valid* complete grid (the second completion that makes the set
    /// unavoidable). This is the contract the catch measurement relies on.
    #[test]
    fn enumerated_uas_are_genuine_unavoidable_sets() {
        for seed in 0..40 {
            let mut rng = Rng::from_seed(seed);
            let sol = random_solution(&mut rng).0;
            let lib = enumerate_2digit_uas(&sol);
            assert!(!lib.uas.is_empty(), "seed {seed}: empty library");
            for (cells, size) in &lib.uas {
                assert_eq!(*size, cells.len());
                assert!(*size >= 4 && *size % 2 == 0, "seed {seed}: odd/small UA size {size}");
                let mut digs: Vec<usize> = cells.iter().map(|&c| sol.get(c).unwrap().index()).collect();
                digs.sort_unstable();
                digs.dedup();
                assert_eq!(digs.len(), 2, "seed {seed}: UA spans {} digits", digs.len());
                let (a, b) = (digs[0], digs[1]);
                let mut swapped = sol.clone();
                for &c in cells {
                    let nd = if sol.get(c).unwrap().index() == a { b } else { a };
                    swapped.set(c, Digit::from_index(nd));
                }
                assert!(is_valid_complete(&swapped), "seed {seed}: UA swap is not a valid grid");
                assert_ne!(swapped.to_line(), sol.to_line(), "seed {seed}: UA swap is the identity");
            }
        }
    }
}
