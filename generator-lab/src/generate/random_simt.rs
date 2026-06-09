//! The attempt **warp** on the `repr` layer: K independent strip attempts driven in
//! lockstep, with per-lane refill. Each lane's per-cell gate logic is the
//! [`random`](crate::generate::random) `StripState` (the incremental
//! [`DualSolverState`](crate::repr::banded::DualSolverState) strip), and the uniqueness
//! and baseline gates it batches both run on the W=8 [`UnifiedWarp`](crate::solve::simt)
//! (the gather-free smear+ALU prober kernel plus the baseline closure, one shared warp).
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
//!   - **expensive (the gate):** the uniqueness prober and the baseline solve. The
//!     pending probe is handed to the [`UnifiedWarp`](crate::solve::simt), which runs it
//!     on one of its 8 SIMD slots alongside seven other lanes; on a unique verdict the
//!     same slot flips probe -> baseline in place (no second warp, no Amdahl half).
//!
//! **Streaming, not a barrier.** The unified warp owns the loop ([`run_warp_unified`] /
//! [`find_puzzles`] drive [`UnifiedWarp::run_stream`](crate::solve::simt::UnifiedWarp)):
//! each SIMD slot streams one logical lane's gates, and the moment a slot reaches a
//! verdict the refill callback resolves it (revert/keep or apply the baseline), walks
//! that lane to its next gate, and hands the new probe straight back — no two-phase
//! advance-all-then-resolve-all barrier, no FIFO-depth knob. A lane that exhausts its
//! quota finalizes (verify) and its slot grabs the next unstarted lane, so at most 8
//! attempts are ever in flight yet the warp stays full while work remains. Logical lanes
//! are independent, so the 8-slot interleave can't change any lane's outcome
//! (`tests/equiv_warp_repr.rs` pins each lane byte-identical to its sequential
//! [`crate::generate::run_attempts`] run).

use super::random::{GeneratedPuzzle, Stats, StripState, cells_u8, verify};
use crate::fill::random_solution;
use crate::probe::simt::{LANES, Probe};
use crate::repr::{CELLS, Digit, DigitGrid, Solution};
use crate::rng::Rng;
use crate::solve::simt::{SolveQuery, UnifiedRefill, UnifiedVerdict, UnifiedWarp};
use crate::spec::Spec;
use crate::spec::kinds::SolveTrace;
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
    /// tagged with so [`find_puzzles`] can return a seed -> puzzle map. (`run_warp_unified`
    /// derives seeds positionally and ignores this.)
    seed: u64,
    /// Attempts still owed by this lane (its share of the fixed work budget). Only the
    /// fixed-work [`run_warp_unified`] consults it; the seed-driven [`find_puzzles`] sets it
    /// to `usize::MAX` (a seed retries until it yields, no per-seed cap).
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

    /// Fixed-work refill (the [`run_warp_unified`] policy): walk to the next gate, and whenever
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
                    on_uniqueness(lane); // revert + clear the gate
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

// ===========================================================================
// Unified warp: probe + baseline gates on one warp
// ===========================================================================
//
// The uniqueness gate (the packed prober) and the *baseline* gate are both per-lane and
// the baseline is downstream of the prober verdict, so a lane parked at a baseline gate
// can't produce its next uniqueness probe until the baseline resolves. Running the
// baseline scalar per lane (a warp-only profile put it at ~46% of warp time) was the
// remaining Amdahl half. The winner is the single-warp unified host
// ([`run_warp_unified`] / [`find_puzzles`]): probe AND baseline lanes share one
// [`UnifiedWarp`], a slot flipping probe -> baseline in place on a unique verdict, with
// no second warp and no oversubscription. It reuses the equiv-tested closure, so per-lane
// verdicts (and thus the produced puzzles / `Stats`) stay byte-identical to the
// sequential generator.

/// Apply a non-unique prober verdict to a lane parked at a gate: revert the strip and
/// clear the gate (the lane is now ready to advance). A unique verdict instead flips the
/// slot to baseline in place at the call site (reusing the cached probe export), so it
/// never routes through here.
fn on_uniqueness(lane: &mut Lane) {
    let (cell, orig, _alts) = lane.pending.expect("on_uniqueness on a lane with no gate");
    lane.strip.revert_gate(cell, orig);
    lane.pending = None;
}

/// Apply a deferred baseline [`SolveTrace`] to a lane that was parked at a unique gate,
/// then clear the gate (the lane is ready to advance).
fn on_baseline(lane: &mut Lane, spec: &Spec, trace: &SolveTrace) {
    let (cell, orig, _alts) = lane.pending.take().expect("on_baseline on a lane with no gate");
    lane.strip.apply_baseline(cell, orig, trace, spec);
}

/// **Unified warp (U).** One [`UnifiedWarp`] runs the uniqueness and baseline gates on the
/// SAME 8 SIMD lanes instead of two coupled warps. A slot stays bound to its macro-lane and
/// flips probe -> baseline *in place* the instant the prober verdict is unique (no batch, no
/// inter-warp queue), then baseline -> probe (advance the strip) when the baseline resolves;
/// when the macro-lane retires the slot grabs the next fresh one. The combined warp is full
/// whenever work of either kind exists and probes are always plentiful, so utilization is
/// ~100% at active set = 8 — no oversubscription. `lanes` is just the total work split into
/// macro-lanes (8 in flight at a time); byte-identical per lane to the sequential
/// [`crate::generate::run_attempts`] run. The kernel is the baseline closure, sound for a
/// probe lane too since extra propagation only prunes the existence search.
pub fn run_warp_unified(base_seed: u64, spec: &Spec, lanes: usize, attempts_per_lane: usize) -> WarpResult {
    let baseline = spec.baseline_mask();

    let mut ls: Vec<Lane> = (0..lanes)
        .map(|l| Lane::new(Rng::from_seed(base_seed + l as u64), attempts_per_lane))
        .collect();
    let mut warp = UnifiedWarp::new();
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
                        on_uniqueness(&mut ls[ll]); // reverts, clears `pending`
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
