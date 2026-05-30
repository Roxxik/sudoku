use crate::board::{Board, CELLS, cell_name, digit_to_bit, iter_digits, popcount};
use crate::rng::Rng;
use crate::solver::{apply_step, next_step_filtered, solve};
use crate::spec::{Spec, Usage};
use crate::techniques::{Deduction, REGISTRY, TechniqueKind};
use crate::uniqueness;
use crate::verifier::verify;

const NUM_TECHNIQUES: usize = REGISTRY.len();

pub struct GeneratedPuzzle {
    pub puzzle: Board,
    pub solution: Board,
    pub givens: usize,
}

pub fn random_full_grid(rng: &mut Rng) -> Board {
    let mut board = Board::empty();
    let ok = fill(&mut board, rng);
    debug_assert!(ok, "fill should always succeed on empty board");
    board
}

fn fill(board: &mut Board, rng: &mut Rng) -> bool {
    let mut best: Option<(usize, u16, u32)> = None;
    for i in 0..CELLS {
        if !board.is_empty(i) {
            continue;
        }
        let cs = board.candidates(i);
        let n = popcount(cs);
        if n == 0 {
            return false;
        }
        if best.map_or(true, |(_, _, bn)| n < bn) {
            best = Some((i, cs, n));
        }
    }
    let Some((cell, mask, _)) = best else {
        return true;
    };
    let mut digits: Vec<u8> = iter_digits(mask).collect();
    rng.shuffle(&mut digits);
    for d in digits {
        let backup = board.clone();
        board.place(cell, d);
        if fill(board, rng) {
            return true;
        }
        *board = backup;
    }
    false
}

pub struct FilteredResult {
    pub puzzle: GeneratedPuzzle,
    pub attempts: usize,
}

pub fn make_puzzle_forced(
    rng: &mut Rng,
    target: TechniqueKind,
    max_attempts: usize,
) -> Option<FilteredResult> {
    for attempt in 1..=max_attempts {
        let solution = random_full_grid(rng);
        let mut puzzle = solution.clone();
        let mut positions: Vec<usize> = (0..CELLS).collect();
        rng.shuffle(&mut positions);

        for i in positions {
            if puzzle.is_empty(i) {
                continue;
            }
            let mut candidate = puzzle.clone();
            candidate.clear(i);
            if uniqueness::count_solutions(&candidate, 2) != 1 {
                continue;
            }
            match solve_and_check_forced(&candidate, target) {
                Some(true) => {
                    let givens = (0..CELLS).filter(|&j| !candidate.is_empty(j)).count();
                    return Some(FilteredResult {
                        puzzle: GeneratedPuzzle {
                            puzzle: candidate,
                            solution,
                            givens,
                        },
                        attempts: attempt,
                    });
                }
                Some(false) => {
                    puzzle = candidate;
                }
                None => {} // not solvable, skip this strip
            }
        }
    }
    None
}

/// Returns Some(true) if the puzzle is technique-solvable AND requires
/// `target` (i.e., is not solvable using only the other techniques).
/// Some(false) if it is solvable without `target`. None if it isn't
/// technique-solvable at all.
fn solve_and_check_forced(board: &Board, target: TechniqueKind) -> Option<bool> {
    let mut b = board.clone();
    loop {
        if b.is_solved() {
            return Some(false);
        }
        match crate::solver::next_step_filtered(&b, |t| t != target) {
            Some(s) => crate::solver::apply_step(&mut b, &s),
            None => {
                // Filtered walk is stuck. The puzzle is solvable iff the
                // canonical solver can finish from here. Soundness of
                // deductions: every filtered step we already applied is a
                // true fact, so canonical-from-here agrees with canonical-
                // from-original on solvability.
                if crate::solver::solve_solvable_only(b) {
                    return Some(true);
                }
                return None;
            }
        }
    }
}

pub fn make_puzzle_needing(
    rng: &mut Rng,
    target: TechniqueKind,
    max_attempts: usize,
) -> Option<FilteredResult> {
    for attempt in 1..=max_attempts {
        let solution = random_full_grid(rng);
        let mut puzzle = solution.clone();
        let mut positions: Vec<usize> = (0..CELLS).collect();
        rng.shuffle(&mut positions);

        for i in positions {
            if puzzle.is_empty(i) {
                continue;
            }
            let mut candidate = puzzle.clone();
            candidate.clear(i);
            if uniqueness::count_solutions(&candidate, 2) != 1 {
                continue;
            }
            let result = solve(candidate.clone());
            if !result.solved {
                continue;
            }
            puzzle = candidate;
            if result.trace.iter().any(|s| s.technique == target) {
                let givens = (0..CELLS).filter(|&j| !puzzle.is_empty(j)).count();
                return Some(FilteredResult {
                    puzzle: GeneratedPuzzle {
                        puzzle,
                        solution,
                        givens,
                    },
                    attempts: attempt,
                });
            }
        }
    }
    None
}

/// Per-row outcome counters for the spec generator. Useful for tuning: a
/// high `requirement_never_fired` says random grids don't reach the target
/// technique at all (bail early is justified); a high `requirement_not_forced`
/// says they do reach it but it remains substitutable (would benefit from
/// targeted backtracking).
#[derive(Debug, Default, Clone)]
pub struct GenStats {
    /// Total grids tried before success / giving up.
    pub attempts: usize,
    /// Attempts where no strip step ever produced a baseline trace meeting the
    /// spec's requirement counts — skipped without running the expensive verify.
    pub requirement_never_fired: usize,
    /// Diagnostic split of `requirement_never_fired`, populated only when the
    /// no-req probe is enabled (`probe_no_req`). The three sum to it.
    ///
    /// The cheap easiest-first pre-filter rejected the seed, but `verify`
    /// (ground truth) would *accept* it — a false negative we discard
    /// unverified. Lever: relax/replace the pre-filter.
    pub no_req_would_verify: usize,
    /// Verify fails, but the target technique IS applicable at some state on
    /// the avoid-target walk — it appears, yet in-scope (incl. tolerated)
    /// techniques route around it. Lever: suppress the substitutes.
    pub no_req_substitutable: usize,
    /// Verify fails, and the target is never applicable anywhere on the
    /// avoid-target walk — the pattern never formed. Lever: create structure.
    pub no_req_absent: usize,
    /// Attempts where the baseline trace met the requirement counts but the
    /// avoid-target walk substituted the requirement away — verify failed.
    pub requirement_not_forced: usize,
    /// Total local-search iterations executed across all attempts.
    pub search_iterations: usize,
    /// Attempts whose final success came from a local-search step (the
    /// greedy-strip output by itself didn't satisfy the spec).
    pub search_recoveries: usize,
    /// Mutations proposed by the heuristic but rejected because of
    /// non-uniqueness / baseline-stuck / no score improvement, respectively.
    /// Together with `search_mutations_accepted` they partition the
    /// proposals the search actually evaluated.
    pub search_rejected_uniqueness: usize,
    pub search_rejected_baseline: usize,
    pub search_rejected_score: usize,
    pub search_mutations_tried: usize,
    pub search_mutations_accepted: usize,
}

/// Generate a puzzle satisfying every constraint expressed in `spec`:
/// solvable using only allowed techniques, every `Forced` technique used at
/// least the required number of times, and every `RequireAny` constraint met.
///
/// Strips cells in random order, reverting any strip that makes the puzzle
/// unsolvable under the baseline or non-unique. During the strip we also
/// track whether the baseline trace already meets the spec's requirement
/// counts — a *necessary* condition for the full `verify` to pass, since
/// the irreplaceability walk has strictly more techniques to substitute with.
/// Only candidates that pass this cheap check get the expensive verify call.
pub fn make_puzzle_for_spec(
    rng: &mut Rng,
    spec: &Spec,
    max_attempts: usize,
) -> Option<FilteredResult> {
    make_puzzle_for_spec_with_stats(rng, spec, max_attempts).0
}

pub fn make_puzzle_for_spec_with_stats(
    rng: &mut Rng,
    spec: &Spec,
    max_attempts: usize,
) -> (Option<FilteredResult>, GenStats) {
    make_puzzle_for_spec_with_search(rng, spec, max_attempts, 0, None, Probe::default())
}

/// Diagnostic probe options for the spec generator (opt-in; meant for the
/// bench, not production). See `GenStats` for the counters `classify` fills.
#[derive(Debug, Default, Clone, Copy)]
pub struct Probe {
    /// Classify each no-req seed into would-verify / substitutable / absent
    /// (runs an extra `verify` + avoid-walk per no-req attempt).
    pub classify: bool,
    /// Dump the seed grid + full avoid-walk trace for the first `trace`
    /// *substitutable* no-req seeds, marking where the target is also
    /// applicable. 0 = off.
    pub trace: usize,
    /// Run targeted-strip forcing on the first `force` *substitutable* no-req
    /// seeds and print the per-round survival-score progression (does it climb
    /// to FORCED, or stall?). 0 = off.
    pub force: usize,
}

/// Same as [`make_puzzle_for_spec_with_stats`] but with a per-attempt local
/// search budget. `search_iters_per_attempt = 0` disables the search and
/// matches the legacy random-only behavior.
///
/// `resume_threshold` gates vicinity retention (carrying a search endpoint
/// over to the next attempt instead of discovering fresh):
/// - `None` — retention off; every attempt discovers fresh (the random-restart
///   baseline). On forcing-limited specs this is best, since fresh strips
///   readily reproduce forcing candidates and re-deepening just wastes attempts.
/// - `Some(t)` — continue from an endpoint iff it strictly climbed *and* scored
///   ≥ `t`. A high `t` (near the avoid-walk-stuck summit, ~1000) deepens only
///   near-forced candidates; `Some(i64::MIN)` resumes from any climbing endpoint.
///   Expected to pay off once retention has something fresh discovery can't
///   cheaply reproduce — i.e. a rare *appearance*.
///
/// `probe` enables diagnostics (see [`Probe`]): classifying every no-req seed
/// into `no_req_*` buckets and/or dumping example traces. Opt-in, for the
/// bench — it runs an extra `verify` + avoid-walk per no-req attempt.
pub fn make_puzzle_for_spec_with_search(
    rng: &mut Rng,
    spec: &Spec,
    max_attempts: usize,
    search_iters_per_attempt: usize,
    resume_threshold: Option<i64>,
    probe: Probe,
) -> (Option<FilteredResult>, GenStats) {
    let mut stats = GenStats::default();
    let req = RequirementSummary::from_spec(spec);
    let heuristic = crate::search::DefaultHeuristic::new(spec);
    let probe_target = crate::search::primary_target(spec);
    let mut trace_left = probe.trace;
    let mut force_left = probe.force;

    // Vicinity retention: a still-climbing search endpoint carried over from
    // the previous attempt, with the solution it was stripped from. `Some`
    // means continue from it; `None` means discover a fresh candidate.
    let mut resume: Option<(Board, Board)> = None;

    for attempt in 1..=max_attempts {
        stats.attempts = attempt;

        // Search origin: continue last attempt's climbing candidate if we
        // kept one (vicinity search — concentrates the iteration budget on
        // the most promising puzzle instead of discarding it), else discover
        // a fresh one by stripping a new random grid.
        let (seed, solution) = if let Some(carried) = resume.take() {
            carried
        } else {
            let solution = random_full_grid(rng);
            let mut puzzle = solution.clone();
            let mut positions: Vec<usize> = (0..CELLS).collect();
            rng.shuffle(&mut positions);

            // Best candidate seen this attempt: the most-stripped state whose
            // baseline trace satisfies the requirement counts. None if no
            // strip step ever produced such a trace.
            let mut best: Option<Board> = None;

            for i in positions {
                if puzzle.is_empty(i) {
                    continue;
                }
                // Strip cell `i` in place and probe; restore it if the strip is
                // rejected. Avoids cloning the whole board per candidate cell —
                // `place(i, orig)` after `clear(i)` reconstructs the exact prior
                // candidates (only this cell's value was removed), and the probes
                // below never mutate `puzzle`. On acceptance the board is already
                // in its stripped state, so there is nothing to copy.
                let orig = puzzle.cell(i);
                puzzle.clear(i);
                // Uniqueness gate, specialized to the strip context (NOT a
                // drop-in for the general count_solutions used elsewhere, e.g.
                // local_search — this relies on a loop-local invariant).
                //
                // The stripped `puzzle` is the previous (unique, its one solution
                // is `solution`) board with exactly one more cell `i` cleared. So
                // `solution` is already a solution of it, and any *second*
                // solution must differ at `i` (a solution agreeing at `i` would
                // also solve the pre-strip board, which is unique). Hence the
                // board is non-unique iff some alternate digit at `i` — any
                // candidate other than its solution value — also completes.
                // Testing just those alternates skips re-deriving the solution
                // we already hold (the whole `i = solution[i]` subtree), so it's
                // strictly less search than count_solutions(.., 2). A cell left
                // as a naked single (no alternates) is proven unique with no
                // backtracking at all.
                let v_bit = digit_to_bit(solution.cell(i));
                let alts = puzzle.candidates(i) & !v_bit;
                let mut non_unique = false;
                if alts != 0 {
                    // Hoist: build the compact solver from the stripped board
                    // once, then clone+place per alternate digit instead of
                    // rebuilding a Board + dense array for every probe.
                    let probe = uniqueness::ExistenceProbe::from_board(&puzzle);
                    for d in iter_digits(alts) {
                        if probe.has_solution_with(i, d) {
                            non_unique = true;
                            break;
                        }
                    }
                }
                if non_unique {
                    puzzle.place(i, orig); // reject: undo the strip
                    continue;
                }
                // Augmented baseline check: solve with baseline techniques and
                // track whether the trace meets the spec's requirement counts.
                let outcome = baseline_solve_tracked(&puzzle, spec, &req);
                if !outcome.solved {
                    puzzle.place(i, orig); // reject: undo the strip
                    continue;
                }
                // Accept: `puzzle` is already in its stripped state.
                if outcome.requirement_met {
                    best = Some(puzzle.clone());
                }
            }

            // Two paths into local search: requirement met during strip but
            // verify fails (vfy-fail), or requirement never met (no-req). In
            // the first case `best` is the natural seed; in the second the
            // final greedy-stripped `puzzle` is our only handle, even though
            // baseline never hit the target on it.
            let (seed, via_best) = match &best {
                Some(b) => (b.clone(), true),
                None => (puzzle.clone(), false),
            };

            if via_best && verify(&seed, spec).is_ok() {
                return success(seed, solution, attempt, stats);
            }
            if via_best {
                stats.requirement_not_forced += 1;
            } else {
                stats.requirement_never_fired += 1;
                if probe.classify || trace_left > 0 || force_left > 0 {
                    let class = classify_no_req(&seed, spec, probe_target);
                    if probe.classify {
                        match class {
                            NoReqClass::WouldVerify => stats.no_req_would_verify += 1,
                            NoReqClass::Substitutable => stats.no_req_substitutable += 1,
                            NoReqClass::Absent => stats.no_req_absent += 1,
                        }
                    }
                    if matches!(class, NoReqClass::Substitutable) {
                        if let Some(t) = probe_target {
                            if trace_left > 0 {
                                dump_substitutable(&seed, spec, t, attempt);
                                trace_left -= 1;
                            }
                            if force_left > 0 {
                                let ft = crate::search::force_by_targeted_strip(
                                    &seed, &solution, spec, t, 200, rng,
                                );
                                dump_force_trace(&ft, attempt);
                                force_left -= 1;
                            }
                        }
                    }
                }
            }
            (seed, solution)
        };

        if search_iters_per_attempt == 0 {
            continue;
        }
        let res = crate::search::local_search(
            seed,
            &solution,
            spec,
            &heuristic,
            rng,
            search_iters_per_attempt,
            |b| verify(b, spec).is_ok(),
        );
        let sstats = res.stats;
        stats.search_iterations += sstats.iterations;
        stats.search_mutations_tried += sstats.mutations_tried;
        stats.search_mutations_accepted += sstats.mutations_accepted;
        stats.search_rejected_uniqueness += sstats.rejected_uniqueness;
        stats.search_rejected_baseline += sstats.rejected_baseline;
        stats.search_rejected_score += sstats.rejected_score;
        if let Some(b) = res.found {
            stats.search_recoveries += 1;
            return success(b, solution, attempt, stats);
        }
        // Keep climbing from this endpoint next attempt iff retention is
        // enabled and the search both made strict upward progress and reached
        // the quality bar; otherwise drop it and fall back to a fresh
        // discovery — deepening an unproductive plateau would just spin.
        if let Some(t) = resume_threshold {
            if res.improved && res.endpoint_score >= t {
                resume = Some((res.endpoint, solution));
            }
        }
    }
    (None, stats)
}

fn success(
    puzzle: Board,
    solution: Board,
    attempts: usize,
    stats: GenStats,
) -> (Option<FilteredResult>, GenStats) {
    let givens = (0..CELLS).filter(|&j| !puzzle.is_empty(j)).count();
    (
        Some(FilteredResult {
            puzzle: GeneratedPuzzle { puzzle, solution, givens },
            attempts,
        }),
        stats,
    )
}

/// Precomputed requirement targets pulled out of a spec, so the per-strip
/// trace check doesn't iterate the full `usages` map every time. Stores
/// counts in arrays indexed by `TechniqueKind` discriminant to avoid hashing
/// in the inner loop.
struct RequirementSummary {
    /// `forced[k_idx]` = minimum trace count required for that kind (0 if not Forced).
    forced: [usize; NUM_TECHNIQUES],
    /// Each entry: bitmask of kinds (1 << k_idx) and the required count.
    require_any: Vec<(u32, usize)>,
    /// Cheap fast-path: if there are no requirements at all, the requirement
    /// check trivially passes after a successful baseline solve.
    is_empty: bool,
}

impl RequirementSummary {
    fn from_spec(spec: &Spec) -> Self {
        let mut forced = [0usize; NUM_TECHNIQUES];
        let mut any_forced = false;
        for (k, usage) in spec.iter_usages() {
            if let Usage::Forced { count } = usage {
                forced[kind_index(k)] = count.get();
                any_forced = true;
            }
        }
        let require_any: Vec<(u32, usize)> = spec
            .require_any
            .iter()
            .map(|r| (r.kinds.bits(), r.count.get()))
            .collect();
        let is_empty = !any_forced && require_any.is_empty();
        Self { forced, require_any, is_empty }
    }
}

#[inline]
fn kind_index(k: TechniqueKind) -> usize {
    k as usize
}

struct BaselineOutcome {
    solved: bool,
    requirement_met: bool,
}

/// Diagnostic classification of a no-req seed (see `GenStats`). Spec-correct:
/// necessity is judged by `verify` (the avoid-walk over the in-scope toolbox),
/// not by the canonical easiest-first solver.
enum NoReqClass {
    /// The verifier accepts the seed even though the cheap pre-filter rejected
    /// it — a false negative we discard unverified.
    WouldVerify,
    /// Verify fails, but the target is applicable at some state along the
    /// avoid-target walk — it appears, yet in-scope techniques route around it.
    Substitutable,
    /// Verify fails and the target is never applicable on the avoid walk.
    Absent,
}

fn classify_no_req(seed: &Board, spec: &Spec, target: Option<TechniqueKind>) -> NoReqClass {
    if verify(seed, spec).is_ok() {
        return NoReqClass::WouldVerify;
    }
    match target {
        Some(t) if target_applicable_on_avoid_path(seed, spec, t) => NoReqClass::Substitutable,
        _ => NoReqClass::Absent,
    }
}

/// Walk the avoid-target path (the same shape `verify` uses: prefer any
/// in-scope non-target step, fall back to the target only when stuck) and
/// report whether the target was *applicable* at any visited state, regardless
/// of whether the walk chose it. Distinguishes "pattern present but routed
/// around" (substitutable) from "pattern never formed" (absent).
fn target_applicable_on_avoid_path(board: &Board, spec: &Spec, target: TechniqueKind) -> bool {
    let mut b = board.clone();
    loop {
        if b.is_solved() {
            return false;
        }
        if next_step_filtered(&b, |t| t == target).is_some() {
            return true;
        }
        // Target not applicable here — advance along an in-scope non-target
        // step. (If none exists the avoid walk is stuck without the target
        // even being an option, so the pattern is absent.)
        match next_step_filtered(&b, |t| spec.is_in_scope(t) && t != target) {
            Some(s) => apply_step(&mut b, &s),
            None => return false,
        }
    }
}

/// Print one substitutable no-req example: the seed grid and the full
/// avoid-target walk, marking every state where the target is *also*
/// applicable (i.e. dodged) and, at the first such state, showing the grid and
/// the target's would-be deductions next to the substitute actually taken. A
/// probe aid — gated by `Probe::trace`.
fn dump_substitutable(seed: &Board, spec: &Spec, target: TechniqueKind, attempt: usize) {
    println!(
        "=== substitutable no-req example (attempt {}) target={:?} diff={} ===",
        attempt,
        target,
        target.difficulty(),
    );
    println!("puzzle: {}", seed.to_line());
    println!("{}", seed.pretty());
    println!("avoid-walk (avoid target; '*' = target also applicable here):");
    let mut b = seed.clone();
    let mut shown_state = false;
    loop {
        if b.is_solved() {
            println!("  (solved without ever being forced to use the target)");
            break;
        }
        let target_step = next_step_filtered(&b, |t| t == target);
        let chosen = next_step_filtered(&b, |t| spec.is_in_scope(t) && t != target)
            .or_else(|| next_step_filtered(&b, |t| spec.is_in_scope(t)));
        let step = match chosen {
            Some(s) => s,
            None => {
                println!("  (stuck — no in-scope step)");
                break;
            }
        };
        let marker = if target_step.is_some() { "*" } else { " " };
        println!(
            "  {} {:?}({})",
            marker,
            step.technique,
            step.technique.difficulty(),
        );
        if let Some(ts) = target_step {
            if !shown_state {
                shown_state = true;
                println!("      target applicable but dodged:");
                println!("        focus: {}", fmt_cells(&ts.focus_cells));
                println!("        would: {}", fmt_deductions(&ts.deductions));
                println!("        substitute taken: {:?}({})", step.technique, step.technique.difficulty());
                println!("        grid at this state:");
                println!("{}", b.pretty());
            }
        }
        apply_step(&mut b, &step);
    }
}

/// Print the per-round survival-score progression of a targeted-strip forcing
/// run — the key signal: does the score climb toward FORCED (1<<30) or stall?
fn dump_force_trace(ft: &crate::search::ForceTrace, attempt: usize) {
    const FORCED: i64 = 1 << 30;
    let render = |s: i64| {
        if s >= FORCED {
            "FORCED".to_string()
        } else {
            s.to_string()
        }
    };
    let seq: Vec<String> = ft
        .rounds
        .iter()
        .map(|r| {
            let mark = if r.accepted { "" } else { "x" };
            format!("{}{}", render(r.score), mark)
        })
        .collect();
    println!(
        "=== force (attempt {}): {} rounds, solved={} ===",
        attempt,
        ft.rounds.len(),
        ft.solved,
    );
    println!("  bottleneck score per round ('x' = no improving strip): [{}]", seq.join(" "));
    if ft.solved {
        println!("  forced puzzle: {}", ft.board.to_line());
    } else if ft.bottleneck.is_empty() {
        println!("  bottleneck: target never applicable on final board");
    } else {
        // The alternatives still keeping the target from being forced — what a
        // smarter neighborhood needs to target.
        let mut dedups: Vec<String> = Vec::new();
        for s in &ft.bottleneck {
            let line = format!(
                "{:?} @ {} -> {}",
                s.technique,
                fmt_cells(&s.focus_cells),
                fmt_deductions(&s.deductions),
            );
            if !dedups.contains(&line) {
                dedups.push(line);
            }
        }
        println!("  bottleneck: {} non-target step(s) to defeat:", dedups.len());
        for line in dedups {
            println!("      {}", line);
        }
    }
}

fn fmt_cells(cells: &[usize]) -> String {
    cells
        .iter()
        .map(|&c| cell_name(c))
        .collect::<Vec<_>>()
        .join(",")
}

fn fmt_deductions(ds: &[Deduction]) -> String {
    ds.iter()
        .map(|d| match d {
            Deduction::Place { cell, digit } => format!("{}={}", cell_name(*cell), digit),
            Deduction::Eliminate { cell, digit } => format!("{}!={}", cell_name(*cell), digit),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Walk the puzzle using baseline techniques only, while tallying how often
/// each technique fires. Returns whether baseline finished AND whether the
/// trace counts met every requirement in `req`. The tally lives in a stack
/// array indexed by kind — no allocation per call.
fn baseline_solve_tracked(
    board: &Board,
    spec: &Spec,
    req: &RequirementSummary,
) -> BaselineOutcome {
    let mut b = board.clone();
    let mut counts = [0usize; NUM_TECHNIQUES];
    // Precompute the baseline membership of every technique once. The filter is
    // queried per technique per solve step (many times); reducing it to an array
    // index here avoids re-hashing the `TechniqueKind` into `spec.usages` on each
    // query. Identical result to calling `spec.is_baseline(t)` directly.
    let mut baseline = [false; NUM_TECHNIQUES];
    for def in REGISTRY {
        baseline[kind_index(def.kind)] = spec.is_baseline(def.kind);
    }
    loop {
        if b.is_solved() {
            let requirement_met = req.is_empty || requirement_met(&counts, req);
            return BaselineOutcome { solved: true, requirement_met };
        }
        match next_step_filtered(&b, |t| baseline[kind_index(t)]) {
            None => return BaselineOutcome { solved: false, requirement_met: false },
            Some(s) => {
                counts[kind_index(s.technique)] += 1;
                apply_step(&mut b, &s);
            }
        }
    }
}

fn requirement_met(counts: &[usize; NUM_TECHNIQUES], req: &RequirementSummary) -> bool {
    for i in 0..NUM_TECHNIQUES {
        if counts[i] < req.forced[i] {
            return false;
        }
    }
    for &(mask, need) in &req.require_any {
        let mut total = 0usize;
        let mut m = mask;
        while m != 0 {
            let i = m.trailing_zeros() as usize;
            total += counts[i];
            m &= m - 1;
        }
        if total < need {
            return false;
        }
    }
    true
}

pub fn make_puzzle(rng: &mut Rng, require_technique_solvable: bool) -> GeneratedPuzzle {
    let solution = random_full_grid(rng);
    let mut puzzle = solution.clone();

    let mut positions: Vec<usize> = (0..CELLS).collect();
    rng.shuffle(&mut positions);

    for i in positions {
        if puzzle.is_empty(i) {
            continue;
        }
        let mut candidate = puzzle.clone();
        candidate.clear(i);
        if uniqueness::count_solutions(&candidate, 2) != 1 {
            continue;
        }
        if require_technique_solvable {
            let result = solve(candidate.clone());
            if !result.solved {
                continue;
            }
        }
        puzzle = candidate;
    }

    let givens = (0..CELLS).filter(|&i| !puzzle.is_empty(i)).count();
    GeneratedPuzzle {
        puzzle,
        solution,
        givens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_grid_is_solved_and_valid() {
        let mut rng = Rng::from_seed(42);
        let grid = random_full_grid(&mut rng);
        assert!(grid.is_solved());
        assert_eq!(uniqueness::count_solutions(&grid, 2), 1);
    }

    #[test]
    fn full_grids_differ_with_different_seeds() {
        let a = random_full_grid(&mut Rng::from_seed(1)).to_line();
        let b = random_full_grid(&mut Rng::from_seed(2)).to_line();
        assert_ne!(a, b);
    }

    #[test]
    fn generated_puzzle_has_unique_solution() {
        let mut rng = Rng::from_seed(123);
        let out = make_puzzle(&mut rng, false);
        assert_eq!(uniqueness::count_solutions(&out.puzzle, 2), 1);
        assert!(out.givens < 81);
    }

    #[test]
    fn technique_solvable_puzzle_actually_solves() {
        let mut rng = Rng::from_seed(456);
        let out = make_puzzle(&mut rng, true);
        let result = solve(out.puzzle.clone());
        assert!(result.solved, "puzzle marked technique-solvable should solve");
        assert_eq!(result.board.to_line(), out.solution.to_line());
    }

    #[test]
    fn make_puzzle_for_spec_train_hidden_pair() {
        let mut rng = Rng::from_seed(123);
        let spec = Spec::train(TechniqueKind::HiddenPair);
        let fr = make_puzzle_for_spec(&mut rng, &spec, 5000)
            .expect("should find a hidden-pair train puzzle within 5000 attempts");
        // Verify the spec is satisfied.
        assert!(verify(&fr.puzzle.puzzle, &spec).is_ok());
        // And solving with all techniques actually finishes.
        let r = solve(fr.puzzle.puzzle.clone());
        assert!(r.solved);
    }

    #[test]
    fn make_puzzle_for_spec_require_family_fish() {
        use crate::techniques::Family;
        let mut rng = Rng::from_seed(2024);
        let spec = Spec::allow_all().require_family(Family::Fish, 1);
        let fr = make_puzzle_for_spec(&mut rng, &spec, 5000)
            .expect("should find a fish-family puzzle within 5000 attempts");
        assert!(verify(&fr.puzzle.puzzle, &spec).is_ok());
    }
}
