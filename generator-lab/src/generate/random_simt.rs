//! The attempt **warp** on the `repr` layer: K independent strip attempts driven in
//! lockstep, with per-lane refill. Each lane's per-cell gate logic is the
//! [`random`](crate::generate::random) `StripState` (the incremental
//! [`DualSolverState`](crate::repr::banded::DualSolverState) strip), and the
//! uniqueness gate it batches is resolved by the packed-DFS prober
//! ([`crate::probe::simt`], the gather-free W=8 smear+ALU kernel).
//!
//! ## The cheap/expensive split and on-demand refill
//!
//! A lane's attempt is [`crate::generate::random::attempt`]'s body, made *resumable*.
//! The work splits into a cheap part and an expensive part:
//!
//!   - **cheap (per-lane, scalar):** walk the shuffled strip order skipping
//!     already-stripped cells and the `alts == 0` fast path (a cell that is still a
//!     naked single — the strip is trivially valid), maintaining the incremental dual
//!     board, until the lane hits a cell whose clear leaves alternatives (`alts != 0`).
//!     That cell needs the **uniqueness gate** — the lane pauses there with its
//!     `(cell, orig, alts)` pending. This is [`Lane::advance_to_gate`].
//!
//!   - **expensive (the gate):** the uniqueness prober + baseline. The pending probe is
//!     handed to the packed prober ([`crate::probe::simt`]), which runs it on one of its
//!     8 SIMD slots alongside seven other lanes' probes. (The baseline gate is still
//!     scalar per-lane — the remaining Amdahl half.)
//!
//! **Streaming, not a barrier.** The packed prober owns the loop ([`run_warp`] drives
//! [`crate::probe::simt::PackedProber::run_stream`]): each SIMD slot streams one logical
//! lane's gates, and the moment a slot reaches a verdict the refill callback resolves it
//! ([`resolve_gate_with`]: revert/keep + baseline), walks that lane to its next gate, and
//! hands the new probe straight back — no two-phase advance-all-then-resolve-all barrier,
//! no FIFO-depth knob. A lane that exhausts its quota finalizes (verify) and its slot
//! grabs the next unstarted lane, so at most 8 attempts are ever in flight yet the warp
//! stays full while work remains. Logical lanes are independent, so the 8-slot interleave
//! can't change any lane's outcome (`tests/equiv_warp_repr.rs` pins each lane
//! byte-identical to its sequential [`crate::generate::run_attempts`] run).

use super::random::{
    GeneratedPuzzle, Stats, StripState, baseline_fast_applicable, cells_u8, verify,
};
use crate::fill::random_solution;
use crate::probe::simt::{LANES, PackedProber, Probe};
use crate::repr::{CELLS, Digit, DigitGrid, Solution};
use crate::rng::Rng;
use crate::solve::simt::{
    PackedSolver, SolveQuery, UnifiedRefill, UnifiedVerdict, UnifiedWarp,
};
use crate::spec::Spec;
use crate::spec::kinds::{KindMask, SolveTrace};
use crate::fingerprint::{FNV_OFFSET, FNV_PRIME, fnv_fold_cells};

/// The result of walking one attempt to its next decision point ([`Lane::step_to_gate`]):
/// either it paused at a uniqueness gate, or the attempt finished with an outcome.
enum Step {
    /// Paused at a uniqueness gate; the lane's `pending` holds `(cell, orig, alts)`.
    Gate,
    /// The attempt finished; `Some` is a verified puzzle, `None` a non-yielding attempt.
    Done(Option<GeneratedPuzzle>),
}

/// One lane = one in-flight attempt plus the running tallies/fingerprint of every
/// attempt this lane has retired. The board state and per-cell gate logic are the
/// shared [`StripState`]; the lane adds the resumable walk (strip order + cursor) and
/// the retired-attempt accounting, kept across slot refills so the attempt can be
/// paused at its uniqueness gate.
struct Lane {
    rng: Rng,
    /// The seed this lane's RNG was created from — the key the produced puzzle is
    /// tagged with so [`find_puzzles`] can return a seed -> puzzle map. (`run_warp`
    /// derives seeds positionally and ignores this.)
    seed: u64,
    /// Attempts still owed by this lane (its share of the fixed work budget). Only the
    /// fixed-work [`run_warp`] consults it; the seed-driven [`find_puzzles`] sets it to
    /// `usize::MAX` (a seed retries until it yields, no per-seed cap).
    remaining: usize,
    /// True while an attempt is in flight (between start and finalize).
    active: bool,
    /// Set when the lane is paused at a uniqueness-gate decision: `(cell, orig, alts)`.
    /// `None` means "needs advancing" (or idle).
    pending: Option<(usize, Digit, u16)>,

    // --- in-flight attempt state (valid iff `active`) ---
    /// The board being stripped + the shared gate logic (the same `StripState` the
    /// sequential `attempt` drives).
    strip: StripState,
    /// The full solution grid this attempt is stripping, kept so a success can report
    /// its solution like the scalar `generate`.
    solution: Solution,
    positions: [usize; CELLS],
    pos_idx: usize,

    // --- retired-attempt accounting ---
    stats: Stats,
    fp: u64,
}

impl Lane {
    fn new(rng: Rng, quota: usize) -> Self {
        Lane {
            rng,
            seed: 0,
            remaining: quota,
            active: false,
            pending: None,
            // Placeholder state; replaced by `start_attempt` before any use.
            strip: StripState::new(&Solution(DigitGrid::EMPTY)),
            solution: Solution(DigitGrid::EMPTY),
            positions: core::array::from_fn(|i| i),
            pos_idx: 0,
            stats: Stats::default(),
            fp: FNV_OFFSET,
        }
    }

    /// Begin a fresh attempt: random full grid, shuffled strip order, reset gate state.
    /// Consumes RNG identically to [`crate::generate::random::attempt`] (full grid fill,
    /// then the strip-order shuffle) so the stream stays faithful.
    fn start_attempt(&mut self) {
        let solution = random_solution(&mut self.rng);
        self.strip = StripState::new(&solution);
        self.solution = solution;
        self.positions = core::array::from_fn(|i| i);
        self.rng.shuffle(&mut self.positions);
        self.pos_idx = 0;
        self.active = true;
        self.pending = None;
        self.remaining -= 1;
        self.stats.attempts += 1;
    }

    /// Bind this lane to a fresh `seed` and begin its first attempt — the entry point
    /// for the seed-driven [`find_puzzles`] host, which hands a slot a new seed each
    /// time its current one yields its puzzle.
    fn assign_seed(&mut self, seed: u64) {
        self.rng = Rng::from_seed(seed);
        self.seed = seed;
        self.start_attempt();
    }

    /// Walk the cheap part of the **current** attempt until the lane either reaches a
    /// uniqueness-gate decision (sets `pending`, returns [`Step::Gate`]) or runs the
    /// attempt out (finalizes it, returns [`Step::Done`] with the outcome). It does NOT
    /// start the next attempt — the caller's refill policy decides that, which is what
    /// lets the fixed-work and seed-driven hosts share this one walk.
    fn step_to_gate(&mut self, spec: &Spec) -> Step {
        loop {
            if self.pos_idx >= CELLS {
                let outcome = self.finalize(spec);
                self.active = false;
                return Step::Done(outcome);
            }
            let i = self.positions[self.pos_idx];
            self.pos_idx += 1;
            let Some(orig) = self.strip.digit_at(i) else {
                continue; // already stripped
            };
            let alts = self.strip.strip(i, orig);
            if alts == 0 {
                self.strip.keep_trivial();
                continue;
            }
            // Reached the batch point.
            self.pending = Some((i, orig, alts));
            return Step::Gate;
        }
    }

    /// Fixed-work refill (the [`run_warp`] policy): walk to the next gate, and whenever
    /// an attempt finishes start the next one while the quota holds, looping until a
    /// gate is reached or the quota runs out (then the lane goes idle). Produced puzzles
    /// are reported via `on_found`.
    fn advance_to_gate<F: FnMut(GeneratedPuzzle)>(&mut self, spec: &Spec, on_found: &mut F) {
        loop {
            match self.step_to_gate(spec) {
                Step::Gate => return,
                Step::Done(outcome) => {
                    if let Some(p) = outcome {
                        on_found(p);
                    }
                    if self.remaining > 0 {
                        self.start_attempt();
                        continue;
                    }
                    return;
                }
            }
        }
    }

    /// Seed-driven refill (the [`find_puzzles`] policy): walk the current seed's
    /// attempts until **one succeeds**, returning that puzzle (the seed's single
    /// output), or park at a uniqueness gate (returns `None`, with `pending` set, so the
    /// host can hand the probe to the packed prober). A failed attempt starts the next
    /// one on the SAME seed's stream — the seed retries until it yields — so the result
    /// is a pure function of the seed, byte-identical to scalar
    /// [`generate`](crate::generate::generate) from that seed.
    fn advance_until_success(&mut self, spec: &Spec) -> Option<GeneratedPuzzle> {
        loop {
            match self.step_to_gate(spec) {
                Step::Gate => return None,
                Step::Done(Some(p)) => return Some(p), // the seed's puzzle
                Step::Done(None) => self.start_attempt(), // retry the same seed
            }
        }
    }

    /// Finalize the current attempt: verify `best`, tally the outcome, fold the
    /// fingerprint — byte-identical to [`crate::generate::run_attempts`]'s per-outcome
    /// bookkeeping (the equivalence test relies on it).
    fn finalize(&mut self, spec: &Spec) -> Option<GeneratedPuzzle> {
        match self.strip.best.take() {
            Some(snap) => {
                if verify(&snap, spec) {
                    self.stats.successes += 1;
                    let givens = snap.digit_count();
                    self.stats.total_givens += givens;
                    fnv_fold_cells(&mut self.fp, &cells_u8(&snap));
                    Some(GeneratedPuzzle {
                        puzzle: crate::repr::Puzzle(snap),
                        solution: self.solution.clone(),
                        givens,
                    })
                } else {
                    self.stats.not_forced += 1;
                    self.fp = self.fp.wrapping_mul(FNV_PRIME);
                    None
                }
            }
            None => {
                self.stats.never_fired += 1;
                self.fp = self.fp.wrapping_mul(FNV_PRIME);
                None
            }
        }
    }
}

/// Finish resolving one lane's pending gate given the already-decided uniqueness verdict
/// `nonunique` (from the packed prober). Mirrors the scalar gate sequence: if non-unique,
/// revert; else baseline solvability + requirement counts; accept or revert. `fast`
/// selects the fused fast path vs the exact engine (see `baseline_fast_applicable`).
fn resolve_gate_with(lane: &mut Lane, spec: &Spec, baseline: KindMask, fast: bool, nonunique: bool) {
    let (i, orig, _alts) = lane.pending.take().expect("resolve_gate on a lane with no pending gate");
    lane.strip.resolve_gate(i, orig, nonunique, spec, baseline, fast);
}

/// Snapshot a lane's pending gate as a [`Probe`] for the packed prober: the dual board's
/// row-major bands + empty mask, the stripped cell, and its alternates.
fn probe_of(lane: &Lane) -> Probe {
    let (cell, _orig, alts) = lane.pending.expect("probe_of on a lane with no gate");
    let (r, unsolved) = lane.strip.export_r();
    Probe { r, unsolved, cell, alts }
}

/// Result of a warp run: aggregate tallies plus the per-lane `(stats, fp)` pairs, the
/// latter for the equivalence cross-check against the sequential generator.
pub struct WarpResult {
    pub stats: Stats,
    pub per_lane: Vec<(Stats, u64)>,
}

/// Run `lanes` independent attempt-streams for `spec`, each doing exactly
/// `attempts_per_lane` attempts from its own seed `base_seed + lane_index`. Total
/// attempts = `lanes * attempts_per_lane`. Fixed-work, deterministic.
///
/// **Streaming refill (no macro-step barrier).** The packed prober owns the loop: its 8
/// SIMD slots each run one logical lane's current uniqueness gate, and when a slot reaches
/// a verdict the refill callback *generates that lane's next gate on demand* — apply the
/// verdict (revert/keep + baseline), walk the strip to the next gate (or finalize the
/// attempt and start the lane's next one), and hand back the new probe. A logical lane is
/// bound to a slot until its whole attempt quota is done, then the slot grabs the next
/// unstarted lane. So at most 8 attempts are ever in flight, yet the warp stays full — no
/// FIFO-depth knob, no oversubscription. Lanes are independent, so each logical lane's
/// `(stats, fp)` is byte-identical to the sequential run from its seed regardless of how
/// the 8 slots interleave.
pub fn run_warp(base_seed: u64, spec: &Spec, lanes: usize, attempts_per_lane: usize) -> WarpResult {
    let baseline = spec.baseline_mask();
    let fast = baseline_fast_applicable(spec);

    let mut ls: Vec<Lane> = (0..lanes)
        .map(|l| Lane::new(Rng::from_seed(base_seed + l as u64), attempts_per_lane))
        .collect();
    let mut prober = PackedProber::new();
    // Which logical lane each SIMD slot is currently streaming, and the next unstarted
    // logical lane to hand out when a slot's lane finishes its quota.
    let mut slot_lane = [usize::MAX; LANES];
    let mut next_lane = 0usize;

    prober.run_stream(|slot, verdict| {
        // Continue the slot's current logical lane: apply the gate verdict, then walk to
        // its next gate. If it produces one, that's this slot's refill.
        if let Some(v) = verdict {
            let ll = slot_lane[slot];
            resolve_gate_with(&mut ls[ll], spec, baseline, fast, v);
            ls[ll].advance_to_gate(spec, &mut |_| {}); // throughput bench: drop puzzles
            if ls[ll].pending.is_some() {
                return Some(probe_of(&ls[ll]));
            }
            // else: logical lane `ll` exhausted its quota — grab a fresh lane below.
        }
        // Bind this slot to the next unstarted logical lane that has a gate.
        loop {
            if next_lane >= ls.len() {
                return None; // no work left: this slot goes idle
            }
            let ll = next_lane;
            next_lane += 1;
            if ls[ll].remaining == 0 {
                continue; // empty quota: nothing to stream
            }
            ls[ll].start_attempt();
            ls[ll].advance_to_gate(spec, &mut |_| {}); // throughput bench: drop puzzles
            if ls[ll].pending.is_some() {
                slot_lane[slot] = ll;
                return Some(probe_of(&ls[ll]));
            }
            // This lane produced no gate at all (all attempts trivially valid); its
            // outcomes were already tallied in `advance_to_gate`. Try the next lane.
        }
    });

    let mut stats = Stats::default();
    let mut per_lane = Vec::with_capacity(lanes);
    for lane in &ls {
        stats.add(&lane.stats);
        per_lane.push((lane.stats, lane.fp));
    }
    WarpResult { stats, per_lane }
}

/// Harvest the **faithful corpus of uniqueness probes** a fixed-work [`run_warp`] would
/// hand the packed prober, for the isolated prober benchmark (`proberbench`). Runs the
/// real generator — strip walk + prober verdicts drive the trajectory exactly as
/// production — but records every [`Probe`] the moment it is handed out. The returned Vec
/// is then replayed through [`PackedProber::resolve`] with no strip/baseline/fill in the
/// loop, isolating the prober's raw throughput.
///
/// Each probe is a self-contained existence query (board + stripped cell + alternates), so
/// replaying the corpus reproduces exactly the prober's work — only the interleaved
/// host-side gate resolution is removed.
pub fn collect_probes(base_seed: u64, spec: &Spec, lanes: usize, attempts_per_lane: usize) -> Vec<Probe> {
    let baseline = spec.baseline_mask();
    let fast = baseline_fast_applicable(spec);

    let mut ls: Vec<Lane> = (0..lanes)
        .map(|l| Lane::new(Rng::from_seed(base_seed + l as u64), attempts_per_lane))
        .collect();
    let mut prober = PackedProber::new();
    let mut slot_lane = [usize::MAX; LANES];
    let mut next_lane = 0usize;
    let mut probes: Vec<Probe> = Vec::new();

    prober.run_stream(|slot, verdict| {
        if let Some(v) = verdict {
            let ll = slot_lane[slot];
            resolve_gate_with(&mut ls[ll], spec, baseline, fast, v);
            ls[ll].advance_to_gate(spec, &mut |_| {});
            if ls[ll].pending.is_some() {
                let p = probe_of(&ls[ll]);
                probes.push(p);
                return Some(p);
            }
        }
        loop {
            if next_lane >= ls.len() {
                return None;
            }
            let ll = next_lane;
            next_lane += 1;
            if ls[ll].remaining == 0 {
                continue;
            }
            ls[ll].start_attempt();
            ls[ll].advance_to_gate(spec, &mut |_| {});
            if ls[ll].pending.is_some() {
                slot_lane[slot] = ll;
                let p = probe_of(&ls[ll]);
                probes.push(p);
                return Some(p);
            }
        }
    });

    probes
}

/// Harvest the **faithful corpus of baseline-gate boards** a fixed-work [`run_warp`]
/// would hand the scalar baseline solver, for the isolated SIMT-baseline-solver
/// benchmark/parity (`baselinebench`, `tests/equiv_baseline_simt`). Runs the real
/// generator — strip walk + prober verdicts drive the trajectory exactly as production
/// — and records the stripped placements grid the moment the *baseline* gate would run
/// on it (i.e. every gate the prober found unique, the only ones the baseline sees).
///
/// Each board is a self-contained partial grid; reconstructing its candidate state
/// (`from_digits`) reproduces exactly the board the per-lane scalar
/// [`FusedLogicSolver`](crate::solve::FusedLogicSolver) solves, so the corpus is the
/// SIMT solver's input distribution with the interleaved host-side strip walk removed.
pub fn collect_baseline_boards(
    base_seed: u64,
    spec: &Spec,
    lanes: usize,
    attempts_per_lane: usize,
) -> Vec<DigitGrid> {
    let baseline = spec.baseline_mask();
    let fast = baseline_fast_applicable(spec);

    let mut ls: Vec<Lane> = (0..lanes)
        .map(|l| Lane::new(Rng::from_seed(base_seed + l as u64), attempts_per_lane))
        .collect();
    let mut prober = PackedProber::new();
    let mut slot_lane = [usize::MAX; LANES];
    let mut next_lane = 0usize;
    let mut boards: Vec<DigitGrid> = Vec::new();

    prober.run_stream(|slot, verdict| {
        if let Some(v) = verdict {
            let ll = slot_lane[slot];
            // A unique gate (`!v`) is one the baseline gate runs on — capture the board
            // it sees (the strip's current placements) before resolution mutates it.
            if !v {
                boards.push(ls[ll].strip.board_digits());
            }
            resolve_gate_with(&mut ls[ll], spec, baseline, fast, v);
            ls[ll].advance_to_gate(spec, &mut |_| {});
            if ls[ll].pending.is_some() {
                return Some(probe_of(&ls[ll]));
            }
        }
        loop {
            if next_lane >= ls.len() {
                return None;
            }
            let ll = next_lane;
            next_lane += 1;
            if ls[ll].remaining == 0 {
                continue;
            }
            ls[ll].start_attempt();
            ls[ll].advance_to_gate(spec, &mut |_| {});
            if ls[ll].pending.is_some() {
                slot_lane[slot] = ll;
                return Some(probe_of(&ls[ll]));
            }
        }
    });

    boards
}

/// Generate **exactly one puzzle per seed** in `seeds`, racing the W=8 **unified warp**
/// ([`UnifiedWarp`]: both the uniqueness and baseline gates on the same 8 SIMD lanes)
/// across the seeds and **streaming** each result to `on_found(seed, puzzle)` the moment
/// it is produced. Each seed is run to its *first* success — the same single puzzle
/// scalar [`generate`](crate::generate::generate) yields from `Rng::from_seed(seed)` —
/// so the relation is a pure `seed -> puzzle` map, independent of how the 8 slots
/// interleave or how many slots there are. Returns the aggregate attempt stats;
/// `stats.successes` == the number of `on_found` calls.
///
/// **The production batch path, on the unified warp.** Eight slot-bound lanes, each a seed
/// in flight; a slot flips probe -> baseline in place on a unique gate (no second warp, no
/// oversubscription — see [`run_warp_unified`]) and rolls to the next seed on a success. So
/// the warp is full whenever seeds remain, at active set = 8. Byte-identical to the
/// sequential generator per seed (the packed baseline solver is pinned to the scalar one),
/// pinned by `tests/equiv_warp_repr::find_puzzles_matches_scalar_per_seed`.
///
/// **Streaming, not collected.** Puzzles are handed to `on_found` as soon as they finish
/// (in warp-completion order, *not* seed order), so a caller can persist/print each one
/// immediately and lose nothing already emitted if the run is interrupted (Ctrl-C). A
/// caller that wants them ordered sorts what it collected.
///
/// `seeds` is an [`IntoIterator`] so a **non-contiguous** seed set plugs straight in —
/// e.g. only the seeds a persisted map does not have a puzzle for yet (pass
/// `base..base+n` for a contiguous batch). The iterator is the work list: each SIMD slot
/// works one seed at a time and, the moment that seed yields its puzzle, pulls the next
/// available seed, keeping all 8 slots full while seeds remain. This is the realistic
/// way to *use* the SIMT prober — batch generation, where there are always >= 8
/// independent seeds in flight.
///
/// Note: a seed retries until it yields (no per-seed attempt cap), matching the
/// "one puzzle per seed" contract. Every emitted puzzle is spec-verified, so safe to
/// feed straight to core's verifier.
pub fn find_puzzles<I, F>(seeds: I, spec: &Spec, mut on_found: F) -> Stats
where
    I: IntoIterator<Item = u64>,
    F: FnMut(u64, GeneratedPuzzle),
{
    let baseline = spec.baseline_mask();
    let mut seeds = seeds.into_iter();

    // One slot-bound lane per SIMD slot; each is (re)assigned seeds pulled from `seeds`
    // as it finishes. `usize::MAX` quota = "retry the current seed until it yields".
    let mut ls: Vec<Lane> =
        (0..LANES).map(|_| Lane::new(Rng::from_seed(0), usize::MAX)).collect();
    let mut warp = UnifiedWarp::new();
    // Per-slot probe cache: a unique gate's baseline query reuses the just-built probe's
    // exported board (the strip is unchanged between), saving a second `export_r`.
    let mut slot_probe = [Probe::EMPTY; LANES];

    warp.run_stream(baseline, |slot, verdict| {
        let lane = &mut ls[slot];
        // 1. Apply the slot's verdict (or take its first seed on the initial fill).
        match verdict {
            None => match seeds.next() {
                Some(s) => lane.assign_seed(s),
                None => return UnifiedRefill::Idle,
            },
            Some(UnifiedVerdict::Probe(nonunique)) => {
                if nonunique {
                    on_uniqueness(lane, true); // revert + clear the gate
                } else {
                    // Unique: flip THIS slot to baseline in place, reusing the cached export.
                    let p = &slot_probe[slot];
                    return UnifiedRefill::Baseline(SolveQuery { r: p.r, unsolved: p.unsolved });
                }
            }
            Some(UnifiedVerdict::Baseline(trace)) => on_baseline(lane, spec, &trace),
        }
        // 2. Drive the current seed to its single puzzle, then roll onto the next seed,
        //    until the lane parks at a gate (hand back the probe) or the seeds run out.
        loop {
            match lane.advance_until_success(spec) {
                None => {
                    let p = probe_of(lane); // parked at a uniqueness gate
                    slot_probe[slot] = p;
                    return UnifiedRefill::Probe(p);
                }
                Some(puzzle) => {
                    on_found(lane.seed, puzzle); // stream it out immediately
                    match seeds.next() {
                        Some(s) => lane.assign_seed(s),
                        None => return UnifiedRefill::Idle, // seed supply drained
                    }
                }
            }
        }
    });

    let mut stats = Stats::default();
    for lane in &ls {
        stats.add(&lane.stats);
    }
    stats
}

/// Per-group tallies for the "no-branch probe => baseline-trivial?" study
/// ([`branch_baseline_corr`]). One instance for the probes that branched and one for
/// those that didn't; the question is whether the no-branch group is ~entirely trivial
/// (solved in one closure pass, no subset => HiddenQuad cannot have fired => req not met),
/// in which case those gates could skip the baseline solve entirely.
#[cfg(feature = "count")]
#[derive(Default, Clone, Copy, Debug)]
pub struct CorrGroup {
    pub gates: u64,
    pub solved: u64,
    pub one_pass: u64,
    pub subset_fired: u64,
    /// solved AND one_pass AND no subset — the "skippable" baseline calls.
    pub trivial: u64,
    pub passes: u64,
}

/// Correlate, per **unique** gate (the only ones the baseline runs on), whether the
/// uniqueness probe had to branch with the baseline solve's outcome. Returns
/// `(no_branch, branched)` tallies. Drives the real warp so the gate distribution is
/// faithful; on each unique verdict it reads the prober's `last_branched` flag and runs
/// the scalar [`FusedLogicSolver`] once (tallying this call's pass count via the `fstat`
/// delta), then applies the verdict so the trajectory continues correctly.
#[cfg(feature = "count")]
pub fn branch_baseline_corr(
    base_seed: u64,
    spec: &Spec,
    lanes: usize,
    attempts_per_lane: usize,
) -> (CorrGroup, CorrGroup) {
    use crate::probe::simt::last_branched;
    use crate::solve::{FusedLogicSolver, Solver, fstat_snapshot};
    use crate::spec::kinds::{NAKED_PAIR, NUM};

    let baseline = spec.baseline_mask();
    let mut ls: Vec<Lane> = (0..lanes)
        .map(|l| Lane::new(Rng::from_seed(base_seed + l as u64), attempts_per_lane))
        .collect();
    let mut prober = PackedProber::new();
    let mut slot_lane = [usize::MAX; LANES];
    let mut next_lane = 0usize;
    let mut nb = CorrGroup::default();
    let mut br = CorrGroup::default();

    prober.run_stream(|slot, verdict| {
        if let Some(v) = verdict {
            let ll = slot_lane[slot];
            let (cell, orig, _alts) = ls[ll].pending.expect("verdict on lane with no gate");
            if v {
                ls[ll].strip.revert_gate(cell, orig);
                ls[ll].pending = None;
            } else {
                // Unique gate: this is a baseline call. Classify by probe-branch and the
                // baseline outcome (passes via fstat[1] delta).
                let branched = last_branched(slot);
                let p0 = fstat_snapshot()[1];
                let trace = FusedLogicSolver::solve_tracked(ls[ll].strip.dual(), baseline);
                let passes = fstat_snapshot()[1] - p0;
                let subset: u64 = (NAKED_PAIR..NUM).map(|k| trace.counts[k] as u64).sum();
                let g = if branched { &mut br } else { &mut nb };
                g.gates += 1;
                g.passes += passes;
                if trace.solved {
                    g.solved += 1;
                }
                if passes <= 1 {
                    g.one_pass += 1;
                }
                if subset > 0 {
                    g.subset_fired += 1;
                }
                if trace.solved && passes <= 1 && subset == 0 {
                    g.trivial += 1;
                }
                ls[ll].pending = None;
                ls[ll].strip.apply_baseline(cell, orig, &trace, spec);
            }
            ls[ll].advance_to_gate(spec, &mut |_| {});
            if ls[ll].pending.is_some() {
                return Some(probe_of(&ls[ll]));
            }
        }
        loop {
            if next_lane >= ls.len() {
                return None;
            }
            let ll = next_lane;
            next_lane += 1;
            if ls[ll].remaining == 0 {
                continue;
            }
            ls[ll].start_attempt();
            ls[ll].advance_to_gate(spec, &mut |_| {});
            if ls[ll].pending.is_some() {
                slot_lane[slot] = ll;
                return Some(probe_of(&ls[ll]));
            }
        }
    });

    (nb, br)
}

// ===========================================================================
// SIMT-baseline integration prototypes
// ===========================================================================
//
// `run_warp` above vectorizes only the *uniqueness* gate (the packed prober) and runs
// the *baseline* gate scalar per lane inside the refill callback (`resolve_gate_with`).
// A warp-only profile puts that scalar baseline at ~46% of warp time, and the packed
// baseline solver ([`crate::solve::simt::PackedSolver`]) is 1.87x (train) / 2.97x (drill)
// faster than it in isolation — so vectorizing the baseline too should give ~1.3x e2e.
//
// The baseline gate is *downstream* of the prober verdict, per lane, and the strip is
// sequential, so a lane parked at a baseline gate can't produce its next uniqueness probe
// until the baseline resolves. Two designs survive the prototyping sweep (the decoupled
// two-warp schedulers — opportunistic pipeline, tick-interleave, ping-pong — all lost and
// were removed): the batched fire-at-8 host ([`run_warp_simt`], a probe warp feeding a
// deferred [`crate::solve::simt::PackedSolver`] batch) and the single-warp unified host
// ([`run_warp_unified`], the winner — probe and baseline share one warp). Both reuse the
// equiv-tested solver / closure, so per-lane verdicts (and thus the produced puzzles /
// `Stats`) stay byte-identical to `run_warp`.

/// Apply the prober's uniqueness verdict to a lane parked at a gate. Non-unique: revert
/// and clear the gate (the lane is now ready to advance). Unique: capture the post-strip
/// placements as a [`SolveQuery`] for the deferred batched baseline solve and leave the
/// gate parked (`pending` kept) — returns `Some(query)` iff unique.
fn on_uniqueness(lane: &mut Lane, nonunique: bool) -> Option<SolveQuery> {
    let (cell, orig, _alts) = lane.pending.expect("on_uniqueness on a lane with no gate");
    if nonunique {
        lane.strip.revert_gate(cell, orig);
        lane.pending = None;
        None
    } else {
        // The strip already has `cell` cleared (advance_to_gate stripped it), so its row
        // view is exactly the board the baseline gate runs on — no rebuild, like `probe_of`.
        let (r, unsolved) = lane.strip.export_r();
        Some(SolveQuery { r, unsolved })
    }
}

/// Apply a deferred baseline [`SolveTrace`] to a lane that was parked at a unique gate,
/// then clear the gate (the lane is ready to advance).
fn on_baseline(lane: &mut Lane, spec: &Spec, trace: &SolveTrace) {
    let (cell, orig, _alts) = lane.pending.take().expect("on_baseline on a lane with no gate");
    lane.strip.apply_baseline(cell, orig, trace, spec);
}

/// Upper bound on in-flight macro-lanes for the production host's fixed parking buffer
/// (no heap Vecs on the hot path — warp width is constant and the buffer is bounded by the
/// lane pool). Plenty for the L<=64 sweep; production runs L=16-32.
const MAX_LANES: usize = 256;

/// **Production host.** The pipelined two-warp design distilled: probe warp runs
/// continuously, baseline gates are captured into a deferred batch and **fired the instant
/// 8 accumulate** (a full baseline warp — no opportunistic small flushes), with fixed
/// parking buffers (no per-flush heap Vec). `lanes` macro-lanes in flight; the minimal
/// hot set is ~16 (8 probe + 8 buffer). Byte-identical per lane to [`run_warp`]. The rare
/// board (~5%) the cheap closure can't finish in the warp is resolved by the packed
/// solver's own scalar subset fallback inside the fire.
pub fn run_warp_simt(base_seed: u64, spec: &Spec, lanes: usize, attempts_per_lane: usize) -> WarpResult {
    assert!(lanes <= MAX_LANES, "lanes {lanes} exceeds MAX_LANES {MAX_LANES}");
    let baseline = spec.baseline_mask();

    let mut ls: Vec<Lane> = (0..lanes)
        .map(|l| Lane::new(Rng::from_seed(base_seed + l as u64), attempts_per_lane))
        .collect();
    let mut prober = PackedProber::new();
    let mut solver = PackedSolver::new();
    let mut slot_lane = [usize::MAX; LANES];
    let mut next_lane = 0usize;

    // Fixed parking buffers — bounded by warp width / lane pool, no heap on the hot path.
    let mut probe_ready = [0usize; MAX_LANES]; // lanes whose baseline resolved, awaiting a probe
    let mut pr_len = 0usize;
    let mut bp_lane = [0usize; LANES]; // the deferred baseline batch (fires at 8)
    let mut bp_query = [SolveQuery::EMPTY; LANES];
    let mut bp_out = [SolveTrace::default(); LANES];
    let mut bp_len = 0usize;

    // The flush: solve the parked batch on the baseline warp, then advance each lane to its
    // next gate (re-queued for the probe) or retire it. A macro to keep it inline with the
    // closure's borrows (single-threaded; warp width constant).
    macro_rules! flush {
        () => {{
            solver.solve(baseline, &bp_query[..bp_len], &mut bp_out[..bp_len]);
            for i in 0..bp_len {
                let ll = bp_lane[i];
                on_baseline(&mut ls[ll], spec, &bp_out[i]);
                ls[ll].advance_to_gate(spec, &mut |_| {});
                if ls[ll].pending.is_some() {
                    probe_ready[pr_len] = ll;
                    pr_len += 1;
                }
            }
            bp_len = 0;
        }};
    }

    prober.run_stream(|slot, verdict| {
        if let Some(v) = verdict {
            let ll = slot_lane[slot];
            match on_uniqueness(&mut ls[ll], v) {
                Some(q) => {
                    bp_lane[bp_len] = ll;
                    bp_query[bp_len] = q;
                    bp_len += 1;
                    if bp_len == LANES {
                        flush!(); // fire-at-8: a full baseline warp
                    }
                }
                None => {
                    ls[ll].advance_to_gate(spec, &mut |_| {});
                    if ls[ll].pending.is_some() {
                        return Some(probe_of(&ls[ll]));
                    }
                }
            }
        }
        loop {
            if pr_len > 0 {
                pr_len -= 1;
                let ll = probe_ready[pr_len];
                slot_lane[slot] = ll;
                return Some(probe_of(&ls[ll]));
            }
            if next_lane < ls.len() {
                let ll = next_lane;
                next_lane += 1;
                if ls[ll].remaining == 0 {
                    continue;
                }
                ls[ll].start_attempt();
                ls[ll].advance_to_gate(spec, &mut |_| {});
                if ls[ll].pending.is_some() {
                    slot_lane[slot] = ll;
                    return Some(probe_of(&ls[ll]));
                }
                continue;
            }
            if bp_len > 0 {
                flush!(); // drain the final partial batch
                continue;
            }
            return None;
        }
    });

    let mut stats = Stats::default();
    let mut per_lane = Vec::with_capacity(lanes);
    for lane in &ls {
        stats.add(&lane.stats);
        per_lane.push((lane.stats, lane.fp));
    }
    WarpResult { stats, per_lane }
}

/// **Unified warp (U).** One [`UnifiedWarp`] runs the uniqueness and baseline gates on the
/// SAME 8 SIMD lanes instead of two coupled warps. A slot stays bound to its macro-lane and
/// flips probe -> baseline *in place* the instant the prober verdict is unique (no batch, no
/// inter-warp queue), then baseline -> probe (advance the strip) when the baseline resolves;
/// when the macro-lane retires the slot grabs the next fresh one. The combined warp is full
/// whenever work of either kind exists and probes are always plentiful, so utilization is
/// ~100% at active set = 8 — no oversubscription. `lanes` is just the total work split into
/// macro-lanes (8 in flight at a time); byte-identical per lane to [`run_warp`]. The kernel
/// is the baseline closure ([`warp_pass_full`]), sound for a probe lane since extra
/// propagation only prunes the existence search.
pub fn run_warp_unified(base_seed: u64, spec: &Spec, lanes: usize, attempts_per_lane: usize) -> WarpResult {
    run_warp_unified_impl(base_seed, spec, lanes, attempts_per_lane, false)
}

/// **Lean-kernel unified warp (experiment #2).** As [`run_warp_unified`] but the engine
/// runs the cheap [`crate::probe::simt`] `warp_pass` (no column hidden singles) for every
/// lane, recovering columns only for the baseline lanes that stall (a masked full-closure
/// pass). The bet: probe lanes (the majority) skip the column ALU. Byte-identical per lane
/// to [`run_warp`] (column recovery keeps baseline verdicts exact). A/B it against the
/// full-kernel [`run_warp_unified`] in `simtbaselinebench`.
pub fn run_warp_unified_lean(base_seed: u64, spec: &Spec, lanes: usize, attempts_per_lane: usize) -> WarpResult {
    run_warp_unified_impl(base_seed, spec, lanes, attempts_per_lane, true)
}

fn run_warp_unified_impl(base_seed: u64, spec: &Spec, lanes: usize, attempts_per_lane: usize, lean: bool) -> WarpResult {
    let baseline = spec.baseline_mask();

    let mut ls: Vec<Lane> = (0..lanes)
        .map(|l| Lane::new(Rng::from_seed(base_seed + l as u64), attempts_per_lane))
        .collect();
    let mut warp = if lean { UnifiedWarp::new_lean() } else { UnifiedWarp::new() };
    let mut slot_lane = [usize::MAX; LANES];
    let mut next_lane = 0usize;
    // Per-slot cache of the probe last loaded into that slot — the strip's exported row
    // view. A unique gate's baseline query is the SAME board (the strip is unchanged
    // between building the probe and its verdict), and a `Probe`'s `r`/`unsolved` are the
    // unrestricted stripped board (the alts restriction is applied at load), so the flip to
    // baseline reuses this verbatim instead of calling `export_r` a second time.
    let mut slot_probe = [Probe::EMPTY; LANES];

    warp.run_stream(baseline, |slot, verdict| {
        // 1. Apply the slot's verdict (skipped on the initial fill) and continue its
        //    macro-lane: a unique probe flips the slot to baseline in place; a non-unique
        //    probe or a resolved baseline advances the strip to the next gate.
        if let Some(v) = verdict {
            let ll = slot_lane[slot];
            let next_probe = match v {
                UnifiedVerdict::Probe(nonunique) => {
                    if nonunique {
                        // Non-unique: revert + clear the gate, then walk to the next gate.
                        on_uniqueness(&mut ls[ll], true); // reverts, clears `pending`
                        ls[ll].advance_to_gate(spec, &mut |_| {});
                        ls[ll].pending.is_some()
                    } else {
                        // Unique: flip to baseline on the SAME slot/macro-lane, reusing the
                        // cached export (no second `export_r`). `pending` stays set for the
                        // later `on_baseline`.
                        let p = &slot_probe[slot];
                        return UnifiedRefill::Baseline(SolveQuery { r: p.r, unsolved: p.unsolved });
                    }
                }
                UnifiedVerdict::Baseline(trace) => {
                    on_baseline(&mut ls[ll], spec, &trace);
                    ls[ll].advance_to_gate(spec, &mut |_| {});
                    ls[ll].pending.is_some()
                }
            };
            if next_probe {
                let p = probe_of(&ls[ll]);
                slot_probe[slot] = p;
                return UnifiedRefill::Probe(p);
            }
            // else the macro-lane exhausted its quota — acquire a fresh one below.
        }
        // 2. Bind this slot to the next unstarted macro-lane that has a gate.
        loop {
            if next_lane >= ls.len() {
                return UnifiedRefill::Idle;
            }
            let ll = next_lane;
            next_lane += 1;
            if ls[ll].remaining == 0 {
                continue;
            }
            ls[ll].start_attempt();
            ls[ll].advance_to_gate(spec, &mut |_| {});
            if ls[ll].pending.is_some() {
                slot_lane[slot] = ll;
                let p = probe_of(&ls[ll]);
                slot_probe[slot] = p;
                return UnifiedRefill::Probe(p);
            }
        }
    });

    let mut stats = Stats::default();
    let mut per_lane = Vec::with_capacity(lanes);
    for lane in &ls {
        stats.add(&lane.stats);
        per_lane.push((lane.stats, lane.fp));
    }
    WarpResult { stats, per_lane }
}
