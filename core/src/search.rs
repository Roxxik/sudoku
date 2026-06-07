//! Local search around a baseline-stripped puzzle. Used by the spec
//! generator when greedy stripping produced a candidate the full
//! irreplaceability check rejected (or none at all).
//!
//! The pluggable piece is [`TargetHeuristic`], which scores a candidate
//! and proposes mutations. The default impl uses only trace-derived
//! signals; per-technique impls add structural awareness. Dispatch is
//! static — pick the heuristic once via [`pick_kind`] at the top of
//! [`make_puzzle_with_search`], then monomorphize the inner loop.

use std::collections::HashSet;

use crate::board::{ALL_DIGITS, Board, CELLS, PEERS, iter_digits};
use crate::rng::Rng;
use crate::solver::{all_techniques_filtered, apply_step, next_step_filtered};
use crate::spec::{Spec, Usage};
use crate::techniques::{Deduction, REGISTRY, Step, TechniqueKind};
use crate::uniqueness;
use crate::verifier::verify;

/// Per-iteration diagnostic about a candidate puzzle: how the baseline
/// trace went, and (if there's a Forced target) how an avoid-target walk
/// went using the full in-scope toolbox. Heuristics read this to score
/// closeness and pick mutations.
pub struct Diag {
    pub baseline_solved: bool,
    pub baseline_trace: Vec<TechniqueKind>,
    /// Hardest technique difficulty seen in the baseline trace, or 0.
    pub baseline_max_diff: u32,
    /// Avoid-walk only run when there's a single Forced target (the
    /// primary case heuristics specialize on). None when no target.
    pub avoid: Option<AvoidWalk>,
}

pub struct AvoidWalk {
    pub finished: bool,
    pub length: usize,
    /// How many times the Forced target itself appeared in the avoid
    /// trace (it can fire — we only avoid it when possible).
    pub target_uses_in_avoid: usize,
    /// The first non-target step the avoid walk fired — the "substitute"
    /// the heuristic should try to disable to make the target irreplaceable.
    /// `None` when no substitute fired (rare; means avoid walk used only
    /// the target or got stuck immediately).
    pub first_substitute: Option<Step>,
    /// Highest substitute difficulty (non-target steps only) the avoid
    /// walk fired. Higher = substitutes are forced to use harder
    /// techniques, which is directional evidence the target is closer
    /// to irreplaceable. 0 if no substitute fired.
    pub max_substitute_diff: u32,
}

/// Run the baseline (Allowed | Forced) walk on `board`, collecting the
/// trace. If a `forced_target` is supplied, also run the avoid-target
/// walk using the full in-scope set.
pub fn diagnose(board: &Board, spec: &Spec, forced_target: Option<TechniqueKind>) -> Diag {
    let (baseline_solved, baseline_trace, baseline_max_diff) = baseline_walk(board, spec);
    let avoid = forced_target.map(|t| avoid_walk(board, spec, t));
    Diag { baseline_solved, baseline_trace, baseline_max_diff, avoid }
}

#[allow(deprecated)] // solver_order: pending difficulty/curriculum migration
fn baseline_walk(board: &Board, spec: &Spec) -> (bool, Vec<TechniqueKind>, u32) {
    let mut b = board.clone();
    let mut trace = Vec::new();
    let mut max_diff = 0u32;
    loop {
        if b.is_solved() {
            return (true, trace, max_diff);
        }
        match next_step_filtered(&b, |t| spec.is_baseline(t)) {
            None => return (false, trace, max_diff),
            Some(s) => {
                let d = s.technique.solver_order();
                if d > max_diff {
                    max_diff = d;
                }
                trace.push(s.technique);
                apply_step(&mut b, &s);
            }
        }
    }
}

#[allow(deprecated)] // solver_order: pending difficulty/curriculum migration
fn avoid_walk(board: &Board, spec: &Spec, target: TechniqueKind) -> AvoidWalk {
    let mut b = board.clone();
    let mut length = 0usize;
    let mut target_uses = 0usize;
    let mut first_substitute: Option<Step> = None;
    let mut max_sub_diff = 0u32;
    loop {
        if b.is_solved() {
            return AvoidWalk {
                finished: true,
                length,
                target_uses_in_avoid: target_uses,
                first_substitute,
                max_substitute_diff: max_sub_diff,
            };
        }
        // Prefer any in-scope technique that isn't the target.
        if let Some(s) = next_step_filtered(&b, |t| spec.is_in_scope(t) && t != target) {
            if first_substitute.is_none() {
                first_substitute = Some(s.clone());
            }
            let d = s.technique.solver_order();
            if d > max_sub_diff {
                max_sub_diff = d;
            }
            apply_step(&mut b, &s);
            length += 1;
            continue;
        }
        // Stuck without target — try target itself.
        match next_step_filtered(&b, |t| spec.is_in_scope(t)) {
            None => {
                return AvoidWalk {
                    finished: false,
                    length,
                    target_uses_in_avoid: target_uses,
                    first_substitute,
                    max_substitute_diff: max_sub_diff,
                };
            }
            Some(s) => {
                if s.technique == target {
                    target_uses += 1;
                }
                apply_step(&mut b, &s);
                length += 1;
            }
        }
    }
}

/// Pick the strongest Forced(T) target from a spec, if any — used as the
/// avoid-walk subject. A spec can force multiple techniques, but the
/// avoid walk targets one; conventionally that's the hardest one.
#[allow(deprecated)] // solver_order: pending difficulty/curriculum migration
pub fn primary_target(spec: &Spec) -> Option<TechniqueKind> {
    let mut best: Option<(TechniqueKind, u32)> = None;
    for (k, u) in spec.iter_usages() {
        if matches!(u, Usage::Forced { .. }) {
            let d = k.solver_order();
            if best.map_or(true, |(_, bd)| d > bd) {
                best = Some((k, d));
            }
        }
    }
    best.map(|(k, _)| k)
}

/// Apply this mutation to a board, returning the new state. The
/// `solution` argument supplies the value for any cell being restored.
#[derive(Debug, Clone, Copy)]
pub enum Mutation {
    /// Clear a currently-given cell.
    Strip(usize),
    /// Re-fill a currently-empty cell with its solution value.
    Restore(usize),
    /// Strip one cell and restore another — a "swap" in one mutation.
    Swap { strip: usize, restore: usize },
    /// Strip one cell, then add back the fewest solution-givens needed to
    /// restore uniqueness. The repin is always uniqueness-preserving by
    /// construction (see [`strip_and_repin`]), so the search's uniqueness
    /// gate effectively never rejects these — remaining rejections become
    /// score-based, i.e. genuine local optima.
    StripRepin(usize),
}

pub fn apply_mutation(board: &Board, solution: &Board, m: Mutation) -> Board {
    let mut b = board.clone();
    match m {
        Mutation::Strip(i) => b.clear(i),
        Mutation::Restore(i) => {
            if b.is_empty(i) {
                let d = solution.cell(i);
                b.place(i, d);
            }
        }
        Mutation::Swap { strip, restore } => {
            if !b.is_empty(strip) {
                b.clear(strip);
            }
            if b.is_empty(restore) {
                let d = solution.cell(restore);
                b.place(restore, d);
            }
        }
        Mutation::StripRepin(i) => return strip_and_repin(board, solution, i),
    }
    b
}

/// Clear `cell`, then add back the fewest solution-givens needed to restore a
/// unique solution. Generalizes the divergence-restore idea to the
/// >2-solution case: stripping a load-bearing given can leave many
/// completions, and a single restore kills only one. We loop — find an
/// alternate completion, restore one cell where it diverges from `solution`
/// (which kills that alternate and introduces no new ones) — until unique.
/// Terminates in at most (initial solution count − 1) restores.
///
/// Every alternate of the stripped board differs from `solution` at `cell`
/// itself (else it would also solve the unstripped board, contradicting its
/// uniqueness), but the minimum sudoku unavoidable set spans ≥4 cells, so a
/// divergence cell other than `cell` always exists — we restore that,
/// preserving the strip's effect rather than just undoing it.
fn strip_and_repin(board: &Board, solution: &Board, cell: usize) -> Board {
    let mut b = board.clone();
    b.clear(cell);
    loop {
        let sols = uniqueness::collect_solutions(&b, 2);
        if sols.len() < 2 {
            return b; // unique (or — not expected from a valid strip — unsolvable)
        }
        // collect_solutions(·, 2) returns `solution` plus one alternate; the
        // alternate is whichever of the two differs from `solution`.
        let alt = match sols
            .iter()
            .find(|s| (0..CELLS).any(|c| s.cell(c) != solution.cell(c)))
        {
            Some(a) => a,
            None => return b,
        };
        let repin = (0..CELLS)
            .find(|&c| c != cell && b.is_empty(c) && alt.cell(c) != solution.cell(c));
        match repin {
            Some(r) => b.place(r, solution.cell(r)),
            None => return b, // no eligible divergence cell — bail (unexpected)
        }
    }
}

/// Heuristic interface used by [`local_search`]. Implementations are
/// resolved at compile time — pass a concrete type to monomorphize the
/// inner loop. The default impl is [`DefaultHeuristic`]; per-technique
/// impls live alongside it in this module.
pub trait TargetHeuristic {
    /// Score the candidate. Higher is better. The local search picks
    /// the strictly-better candidate from each round of proposals.
    fn score(&self, board: &Board, spec: &Spec, diag: &Diag) -> i64;

    /// Propose mutations to try, in priority order. The search applies
    /// each in turn until one improves the score (or none do). An empty
    /// vec signals "I'm out of ideas" — the search will fall through to
    /// a fresh attempt.
    fn propose(
        &self,
        board: &Board,
        solution: &Board,
        diag: &Diag,
        rng: &mut Rng,
    ) -> Vec<Mutation>;
}

/// Generic default. Scores purely from trace signals — no structural
/// knowledge of any specific technique. Mutations are random swaps.
pub struct DefaultHeuristic {
    pub target: Option<TechniqueKind>,
    pub propose_budget: usize,
}

impl DefaultHeuristic {
    pub fn new(spec: &Spec) -> Self {
        Self { target: primary_target(spec), propose_budget: 8 }
    }
}

impl TargetHeuristic for DefaultHeuristic {
    #[allow(deprecated)] // solver_order: pending difficulty/curriculum migration
    fn score(&self, _board: &Board, _spec: &Spec, diag: &Diag) -> i64 {
        if !diag.baseline_solved {
            return i64::MIN / 4;
        }
        let mut s: i64 = 0;
        // Strong: the avoid walk is stuck without the target.
        if let Some(av) = &diag.avoid {
            if !av.finished {
                s += 1_000;
            }
            // Soft: every step the avoid walk took *before* getting stuck
            // is one we'd ideally have eliminated. Shorter = closer to T
            // being irreplaceable.
            s -= av.length as i64 * 5;
            // Bonus for target firing in the avoid trace too — means it
            // genuinely had no alternative.
            s += av.target_uses_in_avoid as i64 * 20;
        }
        // Soft: hardest baseline technique close to (or at) the target.
        // Encourages search to bias toward harder solving.
        if let Some(t) = self.target {
            let target_diff = t.solver_order() as i64;
            let max_diff = diag.baseline_max_diff as i64;
            // closer to target_diff = better; equal = best.
            s -= (target_diff - max_diff).abs();
        }
        // Soft: baseline used target at all.
        if let Some(t) = self.target {
            let count = diag.baseline_trace.iter().filter(|&&k| k == t).count();
            if count > 0 {
                s += 100 + (count as i64) * 5;
            }
        }
        s
    }

    fn propose(
        &self,
        board: &Board,
        _solution: &Board,
        diag: &Diag,
        rng: &mut Rng,
    ) -> Vec<Mutation> {
        let mut out = Vec::with_capacity(self.propose_budget);

        // Directed mutations: if the avoid walk fired a substitute step,
        // prefer strips that are peer-givens pinning a missing candidate at
        // the substitute's focus cells (breaking the substitute's premise).
        // Each is a `StripRepin`, which clears the cell and minimally re-pins
        // uniqueness — so on a baseline-minimal puzzle, where a bare strip
        // almost always leaves multiple solutions, the result is unique by
        // construction regardless of how many alternates the strip opened up.
        if let Some(av) = diag.avoid.as_ref() {
            if let Some(sub) = av.first_substitute.as_ref() {
                let mut strips =
                    candidates_restoring_substitute_focus(board, &sub.focus_cells);
                rng.shuffle(&mut strips);
                self.push_strips(&strips, &mut out);
            }
        }

        // Fill remaining budget with random strips as fallback exploration.
        if out.len() < self.propose_budget {
            let mut givens: Vec<usize> =
                (0..CELLS).filter(|&i| !board.is_empty(i)).collect();
            rng.shuffle(&mut givens);
            self.push_strips(&givens, &mut out);
        }
        out
    }
}

impl DefaultHeuristic {
    /// Push a `StripRepin` for each strip in order, until the budget fills.
    fn push_strips(&self, strips: &[usize], out: &mut Vec<Mutation>) {
        for &strip in strips {
            if out.len() >= self.propose_budget {
                return;
            }
            out.push(Mutation::StripRepin(strip));
        }
    }
}

/// Peer-givens of the substitute's focus cells that pin at least one of
/// the focus cells' currently-missing candidates. Stripping such a peer
/// restores the missing digit as a candidate, breaking the substitute's
/// candidate-distribution premise. Deduplicated.
fn candidates_restoring_substitute_focus(board: &Board, focus: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for &c in focus {
        if !board.is_empty(c) {
            continue;
        }
        let missing = !board.candidates(c) & ALL_DIGITS;
        if missing == 0 {
            continue;
        }
        for d in iter_digits(missing) {
            for &p in &PEERS[c] {
                if board.cell(p) == d && !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// Per-iteration outcome of [`local_search`], for instrumentation.
#[derive(Debug, Default, Clone, Copy)]
pub struct SearchStats {
    pub iterations: usize,
    pub mutations_tried: usize,
    pub mutations_accepted: usize,
    /// Proposed mutation produced a puzzle with 0 or ≥2 solutions.
    pub rejected_uniqueness: usize,
    /// Proposed mutation produced a unique puzzle but baseline couldn't solve it.
    pub rejected_baseline: usize,
    /// Proposed mutation passed uniqueness + baseline but didn't strictly
    /// improve the heuristic score.
    pub rejected_score: usize,
}

/// Outcome of a [`local_search`] run.
pub struct SearchResult {
    /// The first board that satisfied `verify_pass`, if any.
    pub found: Option<Board>,
    pub stats: SearchStats,
    /// The board the search ended on (the highest-scoring one it reached;
    /// equals the seed if no move was ever accepted). Worth continuing from
    /// next attempt iff `improved`.
    pub endpoint: Board,
    /// The heuristic score of `endpoint`. Lets the caller gate retention on
    /// candidate quality (e.g. only continue from near-summit endpoints).
    pub endpoint_score: i64,
    /// Whether the endpoint's score strictly exceeds the seed's starting
    /// score — i.e. the search *climbed* rather than merely wandering a
    /// plateau. The generator uses this to decide whether to continue from
    /// `endpoint` (still climbing → more budget helps) or restart fresh
    /// (stalled → deepening the same plateau would just spin). Sideways moves
    /// keep the search busy almost every iteration, so "did it run out of
    /// iterations" is not a useful signal; strict score gain is.
    pub improved: bool,
}

/// Run local search around `seed`, looking for a verifying puzzle.
/// `verify_pass` is the success criterion (typically the full
/// [`crate::verifier::verify`]); we delegate it so callers can wire in
/// whatever check they want.
pub fn local_search<H: TargetHeuristic>(
    seed: Board,
    solution: &Board,
    spec: &Spec,
    h: &H,
    rng: &mut Rng,
    max_iters: usize,
    verify_pass: impl Fn(&Board) -> bool,
) -> SearchResult {
    let target = primary_target(spec);
    let mut stats = SearchStats::default();
    let mut puzzle = seed;
    let mut diag = diagnose(&puzzle, spec, target);
    let initial_score = h.score(&puzzle, spec, &diag);
    let mut score = initial_score;
    if verify_pass(&puzzle) {
        return SearchResult {
            endpoint: puzzle.clone(),
            endpoint_score: score,
            found: Some(puzzle),
            stats,
            improved: false,
        };
    }
    for _ in 0..max_iters {
        stats.iterations += 1;
        let proposals = h.propose(&puzzle, solution, &diag, rng);
        if proposals.is_empty() {
            break;
        }
        let mut moved = false;
        for m in proposals {
            stats.mutations_tried += 1;
            let candidate = apply_mutation(&puzzle, solution, m);
            if uniqueness::count_solutions(&candidate, 2) != 1 {
                stats.rejected_uniqueness += 1;
                continue;
            }
            let new_diag = diagnose(&candidate, spec, target);
            if !new_diag.baseline_solved {
                stats.rejected_baseline += 1;
                continue;
            }
            let new_score = h.score(&candidate, spec, &new_diag);
            // Accept equal-or-better. The score is coarse (multiples of
            // 5/20/100) and so plateau-heavy; strict `>` would stall the
            // search at the first flat spot. Sideways moves let it traverse
            // a plateau toward an eventual uphill step. Score stays
            // monotonically non-decreasing, and `max_iters` bounds any
            // wandering, so no patience parameter is needed.
            if new_score >= score {
                puzzle = candidate;
                diag = new_diag;
                score = new_score;
                stats.mutations_accepted += 1;
                if verify_pass(&puzzle) {
                    return SearchResult {
                        endpoint: puzzle.clone(),
                        endpoint_score: score,
                        found: Some(puzzle),
                        stats,
                        improved: score > initial_score,
                    };
                }
                moved = true;
                break;
            } else {
                stats.rejected_score += 1;
            }
        }
        if !moved {
            break; // local optimum — not even a sideways move available
        }
    }
    SearchResult {
        found: None,
        stats,
        endpoint: puzzle,
        endpoint_score: score,
        improved: score > initial_score,
    }
}

// ===========================================================================
// Targeted-strip forcing (experimental)
//
// The probe showed the hard techniques (quads, fish) are present-but-
// substitutable: the target appears, but the avoid-walk dodges it until an
// easier deduction *destroys* it (fills one of its defining cells). This
// mechanism tries to make the target *required* by repeatedly defusing those
// destroyers — stripping a given that lets the target's defining cells survive
// longer — hill-climbing on how long the target survives the avoid-walk until
// it survives all the way to a stuck state (forced). Whether this converges
// (vs whack-a-mole) is exactly the open feasibility question.
// ===========================================================================

/// Score sentinel: a state was reached (while avoiding the target) where no
/// non-target deduction is available but the target is — i.e. the target is
/// required. Far above any (negated) deduction count.
const FORCED_SCORE: i64 = 1 << 30;

/// Count the distinct non-target in-scope deductions available on `board`.
/// This is the quantity that must shrink to zero (while the target is still
/// applicable) for the target to be forced — survival of the *target* is
/// irrelevant; drying up the *alternatives* is what matters.
fn nontarget_deduction_count(board: &Board, spec: &Spec, target: TechniqueKind) -> usize {
    let mut set: HashSet<Deduction> = HashSet::new();
    for step in all_techniques_filtered(board, |t| spec.is_in_scope(t) && t != target) {
        for d in step.deductions {
            set.insert(d);
        }
    }
    set.len()
}

/// Walk the avoid-target path and report the *tightest bottleneck*: the
/// minimum number of distinct non-target deductions available at any state
/// where the target is also applicable. `Some(0)` means a state was reached
/// where only the target can progress — forced. `None` means the target was
/// never applicable along the walk (it cannot be forced from here).
///
/// Crucially this ignores how *long* the target survives — a target that is
/// available beside a fat supply of singles scores poorly, exactly as it
/// should, because those singles are an alternative solving path.
fn bottleneck_nontarget(board: &Board, spec: &Spec, target: TechniqueKind) -> Option<usize> {
    let mut b = board.clone();
    let mut best: Option<usize> = None;
    loop {
        if b.is_solved() {
            return best;
        }
        let target_here = next_step_filtered(&b, |t| t == target).is_some();
        if target_here {
            let n = nontarget_deduction_count(&b, spec, target);
            best = Some(best.map_or(n, |m| m.min(n)));
            if n == 0 {
                return Some(0); // only the target can progress — forced
            }
        }
        match next_step_filtered(&b, |t| spec.is_in_scope(t) && t != target) {
            None => return best, // stuck; if target_here, best already captured 0 above
            Some(s) => apply_step(&mut b, &s),
        }
    }
}

/// Higher is better; `FORCED_SCORE` when the target is required. Otherwise the
/// negated bottleneck count, so the hill-climb is driven to shrink the
/// alternative deductions toward zero.
fn forcing_score(board: &Board, spec: &Spec, target: TechniqueKind) -> i64 {
    match bottleneck_nontarget(board, spec, target) {
        Some(0) => FORCED_SCORE,
        Some(n) => -(n as i64),
        None => i64::MIN / 4, // target never applicable — worst
    }
}

/// Givens peering the cells of the *bottleneck competitors* — the strip
/// neighborhood. The probe showed the surviving alternatives live across the
/// board, not around the quad, so we aim strips at *them*: every cell touched
/// by a bottleneck step (its focus and deduction cells), and the givens peering
/// those cells. Stripping such a given widens a competitor's cell, killing the
/// alternative deduction (a 1-hop "what does this competitor depend on"). A
/// strip that instead harms the target lowers the score and gets rejected.
///
/// Currently unused — the forcing loop runs the all-givens neighborhood while
/// we settle viability; this is the smart narrowing to re-enable for speed.
#[allow(dead_code)]
fn bottleneck_peer_givens(board: &Board, spec: &Spec, target: TechniqueKind) -> Vec<usize> {
    let steps = bottleneck_steps(board, spec, target);
    let mut cells: Vec<usize> = Vec::new();
    for s in &steps {
        for &c in &s.focus_cells {
            if !cells.contains(&c) {
                cells.push(c);
            }
        }
        for d in &s.deductions {
            let c = match d {
                Deduction::Place { cell, .. } | Deduction::Eliminate { cell, .. } => *cell,
            };
            if !cells.contains(&c) {
                cells.push(c);
            }
        }
    }
    let mut out: Vec<usize> = Vec::new();
    for &c in &cells {
        for &p in &PEERS[c] {
            if !board.is_empty(p) && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

fn baseline_solves(board: &Board, spec: &Spec) -> bool {
    let mut b = board.clone();
    loop {
        if b.is_solved() {
            return true;
        }
        match next_step_filtered(&b, |t| spec.is_baseline(t)) {
            None => return false,
            Some(s) => apply_step(&mut b, &s),
        }
    }
}

/// One round of the forcing hill-climb, for instrumentation.
pub struct ForceRound {
    /// Score (target survival, or `FORCED_SCORE`) after this round's accepted
    /// strip — or before giving up if none improved.
    pub score: i64,
    /// Whether a uniqueness- and baseline-preserving strip was accepted.
    pub accepted: bool,
}

pub struct ForceTrace {
    pub rounds: Vec<ForceRound>,
    pub solved: bool,
    pub board: Board,
    /// The non-target in-scope steps still available at the final board's
    /// tightest bottleneck — i.e. the alternatives we'd have to kill to force
    /// the target. Empty when forced (`solved`).
    pub bottleneck: Vec<Step>,
}

/// The board state at `board`'s tightest avoid-walk bottleneck (the state
/// minimizing non-target deductions while the target is still applicable).
fn bottleneck_board(board: &Board, spec: &Spec, target: TechniqueKind) -> Option<Board> {
    let mut b = board.clone();
    let mut best: Option<(usize, Board)> = None;
    loop {
        if b.is_solved() {
            break;
        }
        if next_step_filtered(&b, |t| t == target).is_some() {
            let n = nontarget_deduction_count(&b, spec, target);
            if best.as_ref().map_or(true, |(m, _)| n < *m) {
                best = Some((n, b.clone()));
            }
            if n == 0 {
                break;
            }
        }
        match next_step_filtered(&b, |t| spec.is_in_scope(t) && t != target) {
            None => break,
            Some(s) => apply_step(&mut b, &s),
        }
    }
    best.map(|(_, bd)| bd)
}

/// The non-target in-scope steps available at `board`'s bottleneck — the
/// concrete alternatives keeping the target from being forced.
pub fn bottleneck_steps(board: &Board, spec: &Spec, target: TechniqueKind) -> Vec<Step> {
    match bottleneck_board(board, spec, target) {
        Some(bd) => all_techniques_filtered(&bd, |t| spec.is_in_scope(t) && t != target),
        None => Vec::new(),
    }
}

/// Try to make `target` required on `seed` by targeted stripping. Each round:
/// score the current board by how long the target survives the avoid-walk,
/// then try stripping (with uniqueness re-pin) a given peering the target's
/// defining cells, accepting the first strip that strictly improves survival
/// while keeping the puzzle unique and baseline-solvable. Stops on `verify`
/// success, when no strip improves (local optimum), or after `max_rounds`.
pub fn force_by_targeted_strip(
    seed: &Board,
    solution: &Board,
    spec: &Spec,
    target: TechniqueKind,
    max_rounds: usize,
    rng: &mut Rng,
) -> ForceTrace {
    // Allow sideways moves (equal bottleneck) so the climb can traverse a
    // plateau to a spot where a strip *does* shrink the bottleneck — the same
    // medicine that rescued the main search (#2). Strict improvements are
    // preferred; a sideways move is taken only when none improve. A stall
    // budget caps plateau wandering: give up after `PATIENCE` consecutive
    // non-improving (sideways-only) rounds.
    const PATIENCE: usize = 24;
    let mut b = seed.clone();
    let mut score = forcing_score(&b, spec, target);
    let mut rounds = Vec::new();
    let mut stall = 0usize;
    for _ in 0..max_rounds {
        if verify(&b, spec).is_ok() {
            return ForceTrace { rounds, solved: true, board: b, bottleneck: Vec::new() };
        }
        // Type-agnostic neighborhood: every given is a strip candidate. Slow
        // (up to 81 metric evals per round) but it subsumes any per-competitor
        // strip rule — whatever given breaks a competitor is in here. Decisive
        // go/no-go for strip-based forcing; narrow back to a smart neighborhood
        // (e.g. `bottleneck_peer_givens`) for speed once viability is settled.
        let mut strips: Vec<usize> = (0..CELLS).filter(|&i| !b.is_empty(i)).collect();
        rng.shuffle(&mut strips);
        let mut took_strict = false;
        let mut first_sideways: Option<Board> = None;
        for s in strips {
            let cand = apply_mutation(&b, solution, Mutation::StripRepin(s));
            if uniqueness::count_solutions(&cand, 2) != 1 {
                continue;
            }
            if !baseline_solves(&cand, spec) {
                continue;
            }
            let sc = forcing_score(&cand, spec, target);
            if sc > score {
                b = cand;
                score = sc;
                took_strict = true;
                break;
            } else if sc == score && first_sideways.is_none() {
                first_sideways = Some(cand);
            }
        }
        let accepted = if took_strict {
            stall = 0;
            true
        } else if let Some(sw) = first_sideways {
            b = sw; // sideways: score unchanged
            stall += 1;
            true
        } else {
            false
        };
        rounds.push(ForceRound { score, accepted });
        if !accepted {
            break; // true local optimum — not even a sideways move available
        }
        if stall >= PATIENCE {
            break; // wandered a plateau too long without improving
        }
    }
    let solved = verify(&b, spec).is_ok();
    let bottleneck = if solved {
        Vec::new()
    } else {
        bottleneck_steps(&b, spec, target)
    };
    ForceTrace { rounds, solved, board: b, bottleneck }
}

// Suppress unused-import warning when no callers exist yet.
#[allow(dead_code)]
fn _unused_imports_anchor() {
    let _ = REGISTRY;
}
