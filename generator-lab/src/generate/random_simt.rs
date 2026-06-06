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
use crate::solve::simt::{PackedSolver, SolveQuery};
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

/// Generate **exactly one puzzle per seed** in `seeds`, racing the W=8 packed prober
/// across the seeds and **streaming** each result to `on_found(seed, puzzle)` the moment
/// it is produced. Each seed is run to its *first* success — the same single puzzle
/// scalar [`generate`](crate::generate::generate) yields from `Rng::from_seed(seed)` —
/// so the relation is a pure `seed -> puzzle` map, independent of how the 8 slots
/// interleave or how many slots there are. Returns the aggregate attempt stats;
/// `stats.successes` == the number of `on_found` calls.
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
    let fast = baseline_fast_applicable(spec);
    let mut seeds = seeds.into_iter();

    // One slot-bound lane per SIMD slot; each is (re)assigned seeds pulled from `seeds`
    // as it finishes. `usize::MAX` quota = "retry the current seed until it yields".
    let mut ls: Vec<Lane> =
        (0..LANES).map(|_| Lane::new(Rng::from_seed(0), usize::MAX)).collect();
    let mut prober = PackedProber::new();

    prober.run_stream(|slot, verdict| {
        let lane = &mut ls[slot];
        match verdict {
            // Apply the verdict for the gate this slot just probed, then continue below.
            Some(v) => resolve_gate_with(lane, spec, baseline, fast, v),
            // Initial fill for this slot: take its first seed (or idle if none left).
            None => match seeds.next() {
                Some(s) => lane.assign_seed(s),
                None => return None,
            },
        }
        // Drive the current seed to its single puzzle, then roll onto the next seed,
        // until the lane parks at a gate (hand back the probe) or the seeds run out.
        loop {
            match lane.advance_until_success(spec) {
                None => return Some(probe_of(lane)), // parked at a uniqueness gate
                Some(p) => {
                    on_found(lane.seed, p); // stream it out immediately
                    match seeds.next() {
                        Some(s) => lane.assign_seed(s),
                        None => return None, // seed supply drained: free this slot
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
// until the baseline resolves. Vectorizing the baseline therefore needs a producer/
// consumer between two warps plus enough logical lanes in flight (oversubscription) to
// keep both warps fed. On a single core, interleaving the two warps buys nothing over
// running them in decoupled phases (same instruction stream) — the only cost is warp-drain
// bubbles at the phase edges, which large batches amortize. These two prototypes are the
// two natural decoupled schedulers; both reuse the equiv-tested `PackedSolver`, so per-lane
// verdicts (and thus the produced puzzles / `Stats`) stay byte-identical to `run_warp`.

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

/// **Prototype A1 — nested flush.** Same fixed-work contract as [`run_warp`] but with the
/// baseline gate vectorized too. The packed prober owns the outer loop and runs
/// *continuously* (never drained mid-run); a unique gate's board is captured and pushed to
/// a deferred-baseline batch instead of solved inline, and the slot is refilled from a
/// queue of lanes whose baseline already resolved. When that queue empties, the whole
/// deferred batch is flushed through the packed baseline solver in one shot (a big batch =
/// high solver-warp utilization), refilling the queue. `lanes` logical lanes are kept in
/// flight (oversubscription headroom — pick `lanes` >> 8), each an independent seed, so the
/// per-lane outcome is identical to the sequential run regardless of interleave.
pub fn run_warp_pipelined(base_seed: u64, spec: &Spec, lanes: usize, attempts_per_lane: usize) -> WarpResult {
    let baseline = spec.baseline_mask();

    let mut ls: Vec<Lane> = (0..lanes)
        .map(|l| Lane::new(Rng::from_seed(base_seed + l as u64), attempts_per_lane))
        .collect();
    let mut prober = PackedProber::new();
    let mut solver = PackedSolver::new();
    let mut slot_lane = [usize::MAX; LANES];
    let mut next_lane = 0usize;

    // Lanes whose baseline resolved and which now hold a fresh uniqueness probe.
    let mut probe_ready: Vec<usize> = Vec::with_capacity(lanes);
    // Deferred baseline batch: parallel lane-index / board-query Vecs + a reusable verdict
    // buffer, flushed together through the packed solver.
    let mut bp_lane: Vec<usize> = Vec::with_capacity(lanes);
    let mut bp_query: Vec<SolveQuery> = Vec::with_capacity(lanes);
    let mut bp_out: Vec<SolveTrace> = Vec::with_capacity(lanes);

    prober.run_stream(|slot, verdict| {
        // 1. Resolve the slot's current gate (skipped on the initial fill).
        if let Some(v) = verdict {
            let ll = slot_lane[slot];
            match on_uniqueness(&mut ls[ll], v) {
                Some(q) => {
                    // Unique: defer the baseline; the lane is blocked until the flush.
                    bp_lane.push(ll);
                    bp_query.push(q);
                }
                None => {
                    // Non-unique: reverted; keep the slot on this lane through its next gate.
                    ls[ll].advance_to_gate(spec, &mut |_| {});
                    if ls[ll].pending.is_some() {
                        return Some(probe_of(&ls[ll]));
                    }
                    // else the lane exhausted its quota — acquire a new one below.
                }
            }
        }
        // 2. Acquire a probe for this slot. Priority: a lane whose baseline already
        //    resolved (probe_ready), then a fresh logical lane (which lets the deferred
        //    baseline batch keep growing), and only flush the batch when there is no other
        //    way to make progress. Flushing eagerly (before exhausting fresh lanes) would
        //    drain the batch one board at a time, running the W=8 solver at scalar speed —
        //    the whole point is to flush a big batch so the solver warp runs full.
        loop {
            if let Some(ll) = probe_ready.pop() {
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
            if !bp_lane.is_empty() {
                // No ready probes and no fresh lanes left: flush the whole deferred
                // baseline batch at once (a big batch => the solver warp runs full).
                bp_out.clear();
                bp_out.resize(bp_lane.len(), SolveTrace::default());
                solver.solve(baseline, &bp_query, &mut bp_out);
                for (i, &ll) in bp_lane.iter().enumerate() {
                    on_baseline(&mut ls[ll], spec, &bp_out[i]);
                    ls[ll].advance_to_gate(spec, &mut |_| {});
                    if ls[ll].pending.is_some() {
                        probe_ready.push(ll);
                    }
                }
                bp_lane.clear();
                bp_query.clear();
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

/// **Prototype A2 — ping-pong.** Same fixed-work contract as [`run_warp`], baseline
/// vectorized. Alternates two full warp passes: a *prober phase* drains a worklist of
/// lanes-needing-a-uniqueness-probe (routing unique gates into the deferred-baseline batch
/// and re-queuing non-unique lanes' next gates), then a *solver phase* runs the whole
/// deferred batch through the packed baseline solver and re-queues each lane's next gate.
/// Simpler than [`run_warp_pipelined`] (two plain `run_stream` calls, no nested flush) but
/// it drains *both* warps once per cycle, so it pays two warp-drain bubbles per cycle vs
/// the pipelined host's one-per-flush — the A/B that measures whether that matters.
pub fn run_warp_pingpong(base_seed: u64, spec: &Spec, lanes: usize, attempts_per_lane: usize) -> WarpResult {
    let baseline = spec.baseline_mask();

    let mut ls: Vec<Lane> = (0..lanes)
        .map(|l| Lane::new(Rng::from_seed(base_seed + l as u64), attempts_per_lane))
        .collect();
    let mut prober = PackedProber::new();
    let mut solver = PackedSolver::new();

    // Worklist of lanes that need a uniqueness probe (fresh-started or post-baseline).
    let mut probe_ready: Vec<usize> = (0..lanes).collect();
    // Each fresh lane is started + walked to its first gate up front.
    for &ll in &probe_ready {
        ls[ll].start_attempt();
        ls[ll].advance_to_gate(spec, &mut |_| {});
    }
    probe_ready.retain(|&ll| ls[ll].pending.is_some());

    let mut bp_lane: Vec<usize> = Vec::with_capacity(lanes);
    let mut bp_query: Vec<SolveQuery> = Vec::with_capacity(lanes);
    let mut bp_out: Vec<SolveTrace> = Vec::with_capacity(lanes);

    while !probe_ready.is_empty() {
        // --- prober phase: every queued lane through the uniqueness gate ---
        bp_lane.clear();
        bp_query.clear();
        let mut pr_idx = 0usize; // cursor into probe_ready (a lane may re-enter on revert)
        let mut slot_lane = [usize::MAX; LANES];
        prober.run_stream(|slot, verdict| {
            if let Some(v) = verdict {
                let ll = slot_lane[slot];
                match on_uniqueness(&mut ls[ll], v) {
                    Some(q) => {
                        bp_lane.push(ll);
                        bp_query.push(q);
                    }
                    None => {
                        // Non-unique: advance; its next gate re-enters this same phase.
                        ls[ll].advance_to_gate(spec, &mut |_| {});
                        if ls[ll].pending.is_some() {
                            return Some(probe_of(&ls[ll]));
                        }
                    }
                }
            }
            // Hand out the next queued lane's probe.
            while pr_idx < probe_ready.len() {
                let ll = probe_ready[pr_idx];
                pr_idx += 1;
                slot_lane[slot] = ll;
                return Some(probe_of(&ls[ll]));
            }
            None
        });
        probe_ready.clear();

        // --- solver phase: the whole deferred baseline batch at once ---
        if bp_lane.is_empty() {
            break;
        }
        bp_out.clear();
        bp_out.resize(bp_lane.len(), SolveTrace::default());
        solver.solve(baseline, &bp_query, &mut bp_out);
        for (i, &ll) in bp_lane.iter().enumerate() {
            on_baseline(&mut ls[ll], spec, &bp_out[i]);
            ls[ll].advance_to_gate(spec, &mut |_| {});
            if ls[ll].pending.is_some() {
                probe_ready.push(ll);
            }
        }
    }

    let mut stats = Stats::default();
    let mut per_lane = Vec::with_capacity(lanes);
    for lane in &ls {
        stats.add(&lane.stats);
        per_lane.push((lane.stats, lane.fp));
    }
    WarpResult { stats, per_lane }
}
