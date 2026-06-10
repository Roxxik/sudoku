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
//!     `(cell, orig, alts)` pending. This is [`Lane::step_to_gate`].
//!
//!   - **expensive (the gate):** the uniqueness prober and the baseline solve. The
//!     pending probe is handed to the [`UnifiedWarp`](crate::solve::simt), which runs it
//!     on one of its 8 SIMD slots alongside seven other lanes; on a unique verdict the
//!     same slot flips probe -> baseline *in place* (no second warp, no Amdahl half) —
//!     entirely inside the warp now, so the host never routes the flip.
//!
//! ## The host owns the loop ([`PuzzleStream`])
//!
//! [`PuzzleStream`] owns the warp, the 8 slot-bound lanes, and the seed iterator. It is
//! the coroutine surface: [`pump`](PuzzleStream::pump) runs at most `step_count` warp
//! ticks and hands back one of [`Pumped::Found`] (a puzzle popped),
//! [`Pumped::StepCountReached`] (budget spent, nothing this call — re-pump or cancel), or
//! [`Pumped::NoMorePuzzles`] (seeds drained and every lane idle — terminal). Bounding each
//! call by `step_count` keeps it cancelable/resumable from a UI thread: state lives in the
//! struct, so the next `pump` continues exactly where this one stopped, and a rare or even
//! impossible spec can never freeze the caller.
//!
//! One tick is one [`UnifiedWarp::step`]: the freed lanes' verdicts come back as data
//! ([`StepEvents`]), and the host services each (revert a non-unique strip / apply a
//! baseline trace), walks that lane to its next gate, and reloads the slot — no callback.
//! Logical lanes are independent, so the 8-slot interleave can't change any lane's outcome
//! (`tests/equiv_warp_repr.rs` pins each lane byte-identical to its sequential
//! [`crate::generate::run_attempts`] run).

use super::random::{GeneratedPuzzle, Stats, StripState, baseline_fast_applicable, verify};
use crate::fill::random_solution;
use crate::probe::simt::{LANES, Probe};
use crate::repr::{CELLS, Digit, DigitGrid, Solution};
use crate::rng::Rng;
use crate::solve::simt::{GateResult, StepEvents, UnifiedWarp};
use crate::spec::Spec;
use crate::spec::kinds::{KindMask, SolveTrace};
use std::collections::VecDeque;

/// The result of walking one attempt to its next decision point ([`Lane::step_to_gate`]):
/// either it paused at a uniqueness gate, or the attempt finished with an outcome.
enum Step {
    /// Paused at a uniqueness gate; the lane's `pending` holds `(cell, orig, alts)`.
    Gate,
    /// The attempt finished; `Some` is a verified puzzle, `None` a non-yielding attempt.
    Done(Option<GeneratedPuzzle>),
}

/// One lane = one in-flight attempt plus the running tallies of every attempt this lane
/// has retired. The board state and per-cell gate logic are the
/// shared [`StripState`]; the lane adds the resumable walk (strip order + cursor) and
/// the retired-attempt accounting, kept across slot refills so the attempt can be
/// paused at its uniqueness gate.
struct Lane {
    rng: Rng,
    /// The seed this lane's RNG was created from — the key the produced puzzle is tagged
    /// with so a [`PuzzleStream`] can return a seed -> puzzle map.
    seed: u64,
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
}

impl Lane {
    fn new(rng: Rng) -> Self {
        Lane {
            rng,
            seed: 0,
            active: false,
            pending: None,
            // Placeholder state; replaced by `start_attempt` before any use.
            strip: StripState::new(&Solution(DigitGrid::EMPTY)),
            solution: Solution(DigitGrid::EMPTY),
            positions: core::array::from_fn(|i| i),
            pos_idx: 0,
            stats: Stats::default(),
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
        self.stats.attempts += 1;
    }

    /// Bind this lane to a fresh `seed` and begin its first attempt — the entry point
    /// when a slot is handed a new seed (initial fill, or after the current seed yields).
    fn assign_seed(&mut self, seed: u64) {
        self.rng = Rng::from_seed(seed);
        self.seed = seed;
        self.start_attempt();
    }

    /// Walk the cheap part of the **current** attempt until the lane either reaches a
    /// uniqueness-gate decision (sets `pending`, returns [`Step::Gate`]) or runs the
    /// attempt out (finalizes it, returns [`Step::Done`] with the outcome). It does NOT
    /// start the next attempt — the [`PuzzleStream`] refill policy decides that.
    fn step_to_gate(&mut self, spec: &Spec, reforce: bool) -> Step {
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
            // Hidden-single re-force fast path — the twin of [`attempt`]'s (see
            // [`StripState::reforced`]): both gates skippable, verdict carried.
            if reforce && self.strip.reforced(i, orig) != 0 {
                self.strip.keep_trivial();
                continue;
            }
            // Reached the batch point.
            self.pending = Some((i, orig, alts));
            return Step::Gate;
        }
    }

    /// Apply a non-unique probe verdict: revert the strip and clear the gate (the lane is
    /// now ready to advance).
    fn revert_gate(&mut self) {
        let (cell, orig, _alts) = self.pending.expect("revert_gate on a lane with no gate");
        self.strip.revert_gate(cell, orig);
        self.pending = None;
    }

    /// Apply a deferred baseline [`SolveTrace`] at a unique gate, then clear the gate (the
    /// lane is ready to advance).
    fn apply_baseline(&mut self, spec: &Spec, trace: &SolveTrace) {
        let (cell, orig, _alts) = self.pending.take().expect("apply_baseline on a lane with no gate");
        self.strip.apply_baseline(cell, orig, trace, spec);
    }

    /// Finalize the current attempt: verify `best`, tally the outcome — byte-identical to
    /// [`crate::generate::run_attempts`]'s per-outcome bookkeeping.
    fn finalize(&mut self, spec: &Spec) -> Option<GeneratedPuzzle> {
        match self.strip.best.take() {
            Some(snap) => {
                if verify(&snap, spec) {
                    self.stats.successes += 1;
                    let givens = snap.digit_count();
                    self.stats.total_givens += givens;
                    Some(GeneratedPuzzle {
                        puzzle: crate::repr::Puzzle(snap),
                        solution: self.solution.clone(),
                        givens,
                    })
                } else {
                    self.stats.not_forced += 1;
                    None
                }
            }
            None => {
                self.stats.never_fired += 1;
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

/// Harvest a corpus of uniqueness-gate probes from real strip walks — `attempts` scalar
/// strip attempts from `base_seed` — pairing each gate's packed [`Probe`] with the scalar
/// `Search<Bivalue>` verdict (`true` = an alternate completion exists -> non-unique). The
/// reference that [`crate::solve::simt::resolve_probes`] must reproduce, and the prober
/// bench's corpus. Walks faithfully (resolves each gate before the next), so the gate
/// distribution matches production.
pub fn collect_probes(base_seed: u64, spec: &Spec, attempts: usize) -> Vec<(Probe, bool)> {
    let baseline = spec.baseline_mask();
    let fast = baseline_fast_applicable(spec);
    let mut rng = Rng::from_seed(base_seed);
    let mut out = Vec::new();
    for _ in 0..attempts {
        let solution = random_solution(&mut rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        let mut st = StripState::new(&solution);
        for cell in positions {
            let Some(orig) = st.digit_at(cell) else {
                continue; // already stripped
            };
            let alts = st.strip(cell, orig);
            if alts == 0 {
                st.keep_trivial();
                continue; // `alts == 0` fast path: no gate
            }
            if fast && st.reforced(cell, orig) != 0 {
                st.keep_trivial();
                continue; // hidden-single re-force fast path: production issues no probe
            }
            let (r, unsolved) = st.export_r();
            let nonunique = st.scalar_nonunique(cell, orig);
            out.push((Probe { r, unsolved, cell, alts }, nonunique));
            st.resolve_gate(cell, orig, nonunique, spec, baseline, fast);
        }
    }
    out
}

/// What one [`PuzzleStream::pump`] hands back.
pub enum Pumped {
    /// A puzzle surfaced this call (drained from the ready buffer first, else freshly
    /// completed). `(seed, puzzle)`.
    Found(u64, GeneratedPuzzle),
    /// Ran the full `step_count` with nothing to hand back. Re-pump, or cancel.
    StepCountReached,
    /// Seed iterator empty and every lane idle. Terminal — no further pump yields anything.
    NoMorePuzzles,
}

/// The streaming generator: the W=8 [`UnifiedWarp`] plus its 8 slot-bound [`Lane`]s and
/// the seed iterator they pull from. Owns the loop, so [`pump`](Self::pump) is a plain
/// coroutine — no callback. Each slot streams one seed at a time and, the moment that seed
/// yields its puzzle, pulls the next seed, keeping all 8 slots full while seeds remain.
/// Byte-identical per seed to the sequential [`crate::generate::generate`] (the packed
/// baseline solver is pinned to the scalar one).
///
/// There is no attempt budget here — a bench that wants fixed work caps it from the
/// *outside*, stopping once [`stats`](Self::stats)`.attempts` reaches the target (the
/// counter climbs on every attempt, including retries, so this terminates even on a spec
/// that never yields). That keeps the production loop free of any instrumentation.
pub struct PuzzleStream<'a, I> {
    warp: UnifiedWarp,
    lanes: [Lane; LANES],
    seeds: I,
    spec: &'a Spec,
    allowed: KindMask,
    try_lc: bool,
    /// Hidden-single re-force fast path enabled (production: on; off only for the
    /// A/B harness). Pre-AND-ed with [`baseline_fast_applicable`].
    reforce: bool,
    /// Puzzles completed but not yet handed out — a single tick can finish several lanes.
    ready: VecDeque<(u64, GeneratedPuzzle)>,
    /// Reusable per-tick event buffer (alloc-free).
    events: StepEvents,
}

impl<'a, I: Iterator<Item = u64>> PuzzleStream<'a, I> {
    /// Race `seeds` through the warp, one puzzle per seed (each retried until it yields).
    pub fn new(seeds: I, spec: &'a Spec) -> Self {
        Self::new_with(seeds, spec, true)
    }

    /// [`new`](Self::new) with the hidden-single re-force fast path toggleable — the
    /// A/B harness entry (`examples/reforceab`). The fast path is trajectory-invariant
    /// (sound), so both settings produce identical puzzles; only the cost differs.
    pub fn new_with(seeds: I, spec: &'a Spec, reforce: bool) -> Self {
        let allowed = spec.baseline_mask();
        let mut s = PuzzleStream {
            warp: UnifiedWarp::new(),
            lanes: core::array::from_fn(|_| Lane::new(Rng::from_seed(0))),
            seeds,
            spec,
            allowed,
            try_lc: UnifiedWarp::try_lc(allowed),
            reforce: reforce && baseline_fast_applicable(spec),
            ready: VecDeque::new(),
            events: StepEvents::new(),
        };
        // Initial fill: bind a seed to each slot and walk it to its first gate.
        for slot in 0..LANES {
            match s.seeds.next() {
                Some(seed) => {
                    s.lanes[slot].assign_seed(seed);
                    s.refill_slot(slot);
                }
                None => break,
            }
        }
        s
    }

    /// Run at most `step_count` warp ticks (fewer if a puzzle pops or the stream drains).
    /// Returns early with [`Pumped::Found`] the instant a puzzle is available.
    pub fn pump(&mut self, step_count: usize) -> Pumped {
        for _ in 0..step_count {
            if let Some((seed, p)) = self.ready.pop_front() {
                return Pumped::Found(seed, p);
            }
            if !self.warp.any_active() {
                return Pumped::NoMorePuzzles;
            }
            self.tick();
        }
        if let Some((seed, p)) = self.ready.pop_front() {
            return Pumped::Found(seed, p);
        }
        if !self.warp.any_active() {
            return Pumped::NoMorePuzzles;
        }
        Pumped::StepCountReached
    }

    /// One warp tick: a single [`UnifiedWarp::step`], then service each freed lane (apply
    /// its verdict, walk to the next gate, reload the slot).
    fn tick(&mut self) {
        let spec = self.spec;
        self.warp.step(self.allowed, self.try_lc, &mut self.events);
        let n = self.events.as_slice().len();
        for i in 0..n {
            let ev = self.events.as_slice()[i];
            match ev.result {
                GateResult::ProbeNonUnique => self.lanes[ev.slot].revert_gate(),
                GateResult::Baseline(trace) => self.lanes[ev.slot].apply_baseline(spec, &trace),
            }
            self.refill_slot(ev.slot);
        }
    }

    /// Drive the slot's lane to its next uniqueness gate (load the probe and return), or,
    /// when an attempt finishes, emit its puzzle and start the next attempt — retrying the
    /// same seed on a failure, rolling to the next seed on a success — until a gate is
    /// reached or the attempt budget / seed supply runs out (then the lane goes idle).
    fn refill_slot(&mut self, slot: usize) {
        let spec = self.spec;
        let reforce = self.reforce;
        loop {
            match self.lanes[slot].step_to_gate(spec, reforce) {
                Step::Gate => {
                    let p = probe_of(&self.lanes[slot]);
                    self.warp.load_probe(slot, &p);
                    return;
                }
                Step::Done(outcome) => {
                    let retry_same_seed = outcome.is_none();
                    if let Some(puzzle) = outcome {
                        let seed = self.lanes[slot].seed;
                        self.ready.push_back((seed, puzzle));
                    }
                    if retry_same_seed {
                        self.lanes[slot].start_attempt(); // retry the same seed's stream
                    } else {
                        match self.seeds.next() {
                            Some(seed) => self.lanes[slot].assign_seed(seed),
                            None => return, // seed supply drained: lane goes idle
                        }
                    }
                }
            }
        }
    }

    /// Aggregate the per-lane tallies (yield/attempt counts for the summary line).
    pub fn stats(&self) -> Stats {
        let mut stats = Stats::default();
        for lane in &self.lanes {
            stats.add(&lane.stats);
        }
        stats
    }
}
