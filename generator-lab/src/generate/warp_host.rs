//! The SIMT warp host: K independent strip attempts driven in lockstep over the
//! `repr` layer, with per-lane refill. The production puzzle generator on native.
//!
//! It is factored along four roles, so each can vary without touching the others:
//!
//!   - **Kernel** (vector math): [`warp_pass_full`] and the lane load/snapshot
//!     primitives — free functions in [`solve::simt`](crate::solve::simt) /
//!     [`probe::simt`](crate::probe::simt). "Advance all lanes, return masks"; it
//!     does not care what the lanes hold.
//!   - **Job** ([`WarpJob`]): what occupies a warp lane — the per-slot scalar phase
//!     machine. What to do at load, what to do when the pass leaves the lane
//!     solved/dead/stalled (the warp-vs-scalar split lives HERE), and what verdict
//!     it retires with. [`GateJob`] is the production job: the probe -> in-place
//!     baseline flip -> trace lifecycle, owning its branch stack, subset counts,
//!     cached flip query, ladder memo, and spec-derived config.
//!   - **Lane policy** (the strip driver): WHEN to request warp work — the
//!     coroutine yielding [`WarpJob::Request`]s, resumed with [`WarpJob::Verdict`]s
//!     ([`lane_co`], gate-per-strip; an alternative policy is just another
//!     coroutine fn).
//!   - **Host** ([`PuzzleStream`]): pure plumbing, generic over job and lane —
//!     seeds/ready/stats, the pump budget, pass + service-mask + resume loop. It
//!     knows nothing about probing or baselines.
//!
//! The service-eligibility mask (`active & (solved | dead | !changed)`) stays
//! host-side: "this lane did not advance vectorially" is a kernel fact, not job
//! semantics. Everything else lives in [`GateJob`].
//!
//! Logical lanes are independent, so the 8-slot interleave can't change any lane's
//! outcome: the dispatch is monomorphized and the seed -> puzzle map is byte-identical
//! to the sequential scalar [`generate`](super::generate) (`tests/equiv_warp_repr`
//! pins each lane lane-for-lane). An AVX native play, so it (and the `std::simd` warp
//! it batches onto) stays out of the wasm cdylib, which ships the scalar `run_attempts`
//! path.

use super::random::{GeneratedPuzzle, Stats, StripState, baseline_fast_applicable, verify};
use crate::fill::random_solution;
use crate::probe::simt::{Frame, LANES, M, Probe, V, ZERO, load_lane};
use crate::repr::banded::{Bands, RowMajor};
use crate::repr::{CELLS, Puzzle, SolverState};
use crate::rng::Rng;
use crate::solve::simt::{
    GateResult, LadderMemo, SolveQuery, load_query, prober_service, subset_step, uwstat_add,
    warp_pass_full,
};
use crate::spec::Spec;
use crate::spec::kinds::{KindMask, NUM, SolveTrace};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ops::{Coroutine, CoroutineState};
use std::pin::Pin;
use std::rc::Rc;

/// The strip state the warp's lanes carry: a **single row-major view**. The warp
/// consumes only row bands ([`StripState::export_r`]), the gate tests are row-only,
/// and the baseline trace comes back from the warp — so the dual state's column view,
/// maintained on every `clear_clue`/`place_clue`, would be pure waste here. The scalar
/// [`attempt`](super::random::attempt) (whose baseline gate reads both views) keeps the
/// dual default.
pub type RowStrip = SolverState<Bands<RowMajor>>;

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

/// The warp's resident SoA boards: nine per-digit candidate bands plus the empty
/// mask, eight lanes wide. The kernel advances them in lockstep; a job's scalar
/// service mutates one lane's columns.
pub struct WarpBoards {
    pub r: [[V; 3]; 9],
    pub unsolved: [V; 3],
}

impl WarpBoards {
    fn new() -> Self {
        WarpBoards { r: [[ZERO; 3]; 9], unsolved: [ZERO; 3] }
    }
}

/// What occupies a warp lane: the per-slot scalar phase machine between the lane
/// coroutine's request and its verdict. The host loads a request, runs the shared
/// vector pass, and calls [`service`](Self::service) whenever the pass leaves the
/// lane solved/dead/unchanged; the job advances its phases in place (branch,
/// backtrack, flip, ladder step, ...) until it retires with a verdict, which the
/// host resumes the lane with.
///
/// A job is a data machine on purpose: it is serviced ~every tick (hot) and needs
/// the shared boards, so it cannot be a coroutine (the lending problem) — this
/// trait is the named, swappable form of that machine.
pub trait WarpJob {
    /// What a lane asks for (the yield type of its coroutine).
    type Request;
    /// What a finished job hands back (the resume type of the lane coroutine).
    type Verdict;
    /// The dummy verdict for a lane's very first resume — it has not yielded a
    /// request yet, so the value is never read. A protocol artifact of priming.
    const PRIME: Self::Verdict;
    /// Begin a new job on lane `l` for `req` (the slot was just freed or primed).
    fn load(&mut self, b: &mut WarpBoards, l: usize, req: &Self::Request);
    /// The pass left lane `l` solved/dead/unchanged: advance the job's scalar
    /// state. `None` = still running (rejoin the warp); `Some` = retired.
    fn service(&mut self, b: &mut WarpBoards, l: usize, solved: bool, dead: bool)
    -> Option<Self::Verdict>;
}

/// The production job: one uniqueness gate, probe phase first, flipping to the
/// baseline phase in place on a unique verdict (reusing the cached raw query — a
/// [`Probe`]'s board IS the baseline query, the strip is unchanged between probe
/// and verdict), so the whole gate costs the lane a single suspension. Owns the
/// per-slot search state plus the spec-derived config.
pub struct GateJob {
    /// `true` = baseline phase, `false` = probe phase.
    baseline: bool,
    /// Probe-phase branch stack (unused in the baseline phase).
    stack: Vec<Frame>,
    /// Baseline-phase subset-kind counts (reset on each flip).
    counts: [u16; NUM],
    /// The raw (pre-restriction) probe board, kept for the in-place flip.
    query: SolveQuery,
    /// Cross-stall subset-ladder memo (invalidated on every load/flip).
    memo: LadderMemo,
    // --- spec-derived config (per job; a few bytes) ---
    allowed: KindMask,
    /// Use the cross-stall ladder memo (production: on; off only for A/B).
    ladder_memo: bool,
}

impl GateJob {
    pub fn new(spec: &Spec, ladder_memo: bool) -> Self {
        GateJob {
            baseline: false,
            stack: Vec::with_capacity(64),
            counts: [0; NUM],
            query: SolveQuery::EMPTY,
            memo: LadderMemo::INVALID,
            allowed: spec.baseline_mask(),
            ladder_memo,
        }
    }

    /// Unique probe verdict: flip this lane to the baseline phase in place. The
    /// lane stays active and takes its first baseline pass next tick (the host's
    /// service mask is a snapshot — load-bearing for fingerprint identity).
    #[inline]
    fn flip_to_baseline(&mut self, b: &mut WarpBoards, l: usize) {
        load_query(&mut b.r, &mut b.unsolved, l, &self.query);
        self.counts = [0; NUM];
        self.baseline = true;
        self.memo.invalidate();
    }

    fn trace(&self, solved: bool) -> GateResult {
        GateResult::Baseline(SolveTrace { solved, counts: self.counts })
    }
}

impl WarpJob for GateJob {
    type Request = Probe;
    type Verdict = GateResult;
    const PRIME: GateResult = GateResult::ProbeNonUnique;

    #[inline]
    fn load(&mut self, b: &mut WarpBoards, l: usize, p: &Probe) {
        load_lane(&mut b.r, &mut b.unsolved, l, p);
        self.query = SolveQuery { r: p.r, unsolved: p.unsolved };
        self.stack.clear();
        self.baseline = false;
        self.memo.invalidate();
    }

    // Inline so the per-tick service collapses into the host's loop exactly as the
    // pre-factoring tick had it — outlined, the fat `Option<GateResult>` return
    // travels through memory on every serviced lane (measured ~2% e2e).
    #[inline]
    fn service(&mut self, b: &mut WarpBoards, l: usize, solved: bool, dead: bool)
    -> Option<GateResult> {
        if self.baseline {
            if solved {
                Some(self.trace(true))
            } else if dead {
                Some(self.trace(false))
            } else {
                let memo = self.ladder_memo.then_some(&mut self.memo);
                match subset_step(&mut b.r, &mut b.unsolved, l, self.allowed, memo) {
                    Some(k) => {
                        self.counts[k] = self.counts[k].saturating_add(1);
                        None
                    }
                    None => Some(self.trace(false)),
                }
            }
        } else {
            // Probe phase: the shared prober service decides the uniqueness verdict.
            match prober_service(&mut b.r, &mut b.unsolved, &mut self.stack, l, solved, dead) {
                Some(true) => Some(GateResult::ProbeNonUnique), // a completion exists
                Some(false) => {
                    self.flip_to_baseline(b, l); // tree exhausted: unique
                    None
                }
                None => None, // branched / backtracked: keep searching
            }
        }
    }
}

/// The attempt-boundary channel between the host and its lane coroutines: the seed
/// supply the lanes pull from, the puzzles they push, and the retired-attempt
/// tallies. One `Rc<RefCell<..>>` shared by all eight lanes and the host; touched
/// once per attempt / per found puzzle, never per tick.
pub(in crate::generate) struct Shared<I> {
    pub(in crate::generate) seeds: I,
    /// Puzzles completed but not yet handed out — a single tick can finish several
    /// lanes. `(seed, puzzle)`.
    pub(in crate::generate) ready: VecDeque<(u64, GeneratedPuzzle)>,
    pub(in crate::generate) stats: Stats,
}

pub(in crate::generate) type SharedRef<I> = Rc<RefCell<Shared<I>>>;

/// One slot's lane: the whole resumable strip attempt — seeds loop, retry loop,
/// strip walk, finalize — as a coroutine. Suspends only at uniqueness gates,
/// yielding the gate's [`Probe`] and resuming with its [`GateResult`]. The TAIT
/// names the closure's unnameable type so [`PuzzleStream`] holds its lanes inline
/// (the coroutine is non-`static`, hence `Unpin`: no boxing, `Pin::new` suffices).
pub type LaneCo<I: Iterator<Item = u64>> = impl Coroutine<GateResult, Yield = Probe, Return = ()>;

/// Build one lane coroutine. The body is [`attempt`](super::random::attempt) made
/// literal: compare gate for gate — the `alts == 0` and re-force fast paths, the
/// probe restriction, and the `best`/verify finalize are the shared [`StripState`]
/// logic, so the sequential and SIMT drivers still cannot drift. The first resume's
/// argument is a dummy (the lane has not yielded a gate yet); every later resume
/// carries the verdict of the gate the lane is suspended at.
#[define_opaque(LaneCo)]
pub(in crate::generate) fn lane_co<I: Iterator<Item = u64>>(
    shared: SharedRef<I>,
    spec: Spec,
) -> LaneCo<I> {
    // The hidden-single re-force fast path is sound only when the fused baseline fast
    // path applies (a forced cheap kind reads exact cheap counts the skipped
    // re-derivation would shift by one); the gate matches scalar [`attempt`]. Computed
    // once per lane.
    let fast = baseline_fast_applicable(&spec);
    #[coroutine]
    move |_first: GateResult| {
        loop {
            // New seed: one fresh RNG stream, retried until it yields a puzzle.
            let next = shared.borrow_mut().seeds.next();
            let Some(seed) = next else { return };
            let mut rng = Rng::from_seed(seed);
            loop {
                shared.borrow_mut().stats.attempts += 1;
                let solution = random_solution(&mut rng);
                let mut strip: StripState<RowStrip> = StripState::new(&solution);
                let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
                rng.shuffle(&mut positions);
                for idx in 0..CELLS {
                    let cell = positions[idx];
                    let Some(orig) = strip.digit_at(cell) else {
                        continue; // already stripped
                    };
                    let alts = strip.strip(cell, orig);
                    if alts == 0 {
                        strip.keep_trivial();
                        continue;
                    }
                    // Hidden-single re-force fast path: both gates skippable,
                    // verdict carried (see [`StripState::reforced`]).
                    if fast && strip.reforced(cell, orig) != 0 {
                        strip.keep_trivial();
                        continue;
                    }
                    // The uniqueness gate: suspend here; the warp decides. A unique
                    // verdict comes back as the baseline trace directly (the warp
                    // flips probe -> baseline in place), so one suspension covers
                    // both gates.
                    let (r, unsolved) = strip.export_r();
                    let verdict = yield Probe { r, unsolved, cell, alts };
                    match verdict {
                        GateResult::ProbeNonUnique => strip.revert_gate(cell, orig),
                        GateResult::Baseline(trace) => {
                            strip.apply_baseline(cell, orig, &trace, &spec)
                        }
                    }
                }
                // Finalize — byte-identical bookkeeping to `run_attempts`.
                let success = match strip.best.take() {
                    Some(snap) => {
                        if verify(&snap, &spec) {
                            let givens = snap.digit_count();
                            let mut sh = shared.borrow_mut();
                            sh.stats.successes += 1;
                            sh.stats.total_givens += givens;
                            sh.ready.push_back((
                                seed,
                                GeneratedPuzzle { puzzle: Puzzle(snap), solution, givens },
                            ));
                            true
                        } else {
                            shared.borrow_mut().stats.not_forced += 1;
                            false
                        }
                    }
                    None => {
                        shared.borrow_mut().stats.never_fired += 1;
                        false
                    }
                };
                if success {
                    break; // this seed has its puzzle: pull the next
                }
            }
        }
    }
}

/// One slot: its job (the warp-side machine) and its lane (the strip driver
/// coroutine). The pair fixes the request/verdict protocol by type.
struct Slot<J, C> {
    job: J,
    lane: C,
}

/// The streaming generator: generic plumbing over a [`WarpJob`] and a lane coroutine
/// speaking its protocol. Owns the boards, the active mask, the eight slots, and the
/// attempt-boundary [`Shared`] channel; `pump`/`stats` are the public face.
///
/// There is no attempt budget here: a bench that wants fixed work caps it from the
/// outside on [`stats`](Self::stats)`.attempts` (the counter climbs on every attempt,
/// including retries, so it terminates even on a spec that never yields).
pub struct PuzzleStream<I, J, C>
where
    I: Iterator<Item = u64>,
    J: WarpJob,
    C: Coroutine<J::Verdict, Yield = J::Request, Return = ()> + Unpin,
{
    boards: WarpBoards,
    active: [bool; LANES],
    slots: [Slot<J, C>; LANES],
    shared: SharedRef<I>,
}

/// The production instantiation: [`GateJob`] driven by the gate-per-strip lane
/// ([`lane_co`]) on the row-major [`RowStrip`].
pub type GateStream<I> = PuzzleStream<I, GateJob, LaneCo<I>>;

impl<I, J, C> PuzzleStream<I, J, C>
where
    I: Iterator<Item = u64>,
    J: WarpJob,
    C: Coroutine<J::Verdict, Yield = J::Request, Return = ()> + Unpin,
{
    /// Assemble a stream from its parts: per-slot jobs and lanes from the
    /// factories, lanes primed in slot order (which fixes the seed-pull order).
    /// Generate-internal: experiments pair a job with a lane here; the public
    /// entries pin production pairs (e.g. [`GateStream`]'s `new`).
    pub(in crate::generate) fn assemble(
        seeds: I,
        mut mk_job: impl FnMut() -> J,
        mut mk_lane: impl FnMut(&SharedRef<I>) -> C,
    ) -> Self {
        let shared: SharedRef<I> = Rc::new(RefCell::new(Shared {
            seeds,
            ready: VecDeque::new(),
            stats: Stats::default(),
        }));
        let mut s = PuzzleStream {
            boards: WarpBoards::new(),
            active: [false; LANES],
            slots: core::array::from_fn(|_| Slot { job: mk_job(), lane: mk_lane(&shared) }),
            shared,
        };
        for slot in 0..LANES {
            s.advance(slot, J::PRIME);
        }
        s
    }

    /// Run at most `step_count` warp ticks (fewer if a puzzle pops or the stream
    /// drains). Returns early with [`Pumped::Found`] the instant a puzzle is
    /// available.
    pub fn pump(&mut self, step_count: usize) -> Pumped {
        for _ in 0..step_count {
            if let Some((seed, p)) = self.shared.borrow_mut().ready.pop_front() {
                return Pumped::Found(seed, p);
            }
            if !self.any_active() {
                return Pumped::NoMorePuzzles;
            }
            self.tick();
        }
        if let Some((seed, p)) = self.shared.borrow_mut().ready.pop_front() {
            return Pumped::Found(seed, p);
        }
        if !self.any_active() {
            return Pumped::NoMorePuzzles;
        }
        Pumped::StepCountReached
    }

    fn any_active(&self) -> bool {
        self.active.iter().any(|&a| a)
    }

    /// One warp tick: a single kernel pass over the active lanes, then service
    /// each lane the pass left solved/dead/unchanged. The `service` bitmask is a
    /// snapshot, so a job whose phase flips mid-tick is not re-serviced until the
    /// next pass.
    fn tick(&mut self) {
        let active_mask = M::from_array(self.active);
        if !active_mask.any() {
            return;
        }
        let active_b = active_mask.to_bitmask();
        uwstat_add(0, 1);
        uwstat_add(1, active_b.count_ones() as u64);
        let (changed, dead, solved) =
            warp_pass_full(&mut self.boards.r, &mut self.boards.unsolved, active_mask);

        let solved_b = solved.to_bitmask();
        let dead_b = dead.to_bitmask();
        let changed_b = changed.to_bitmask();
        let mut service = active_b & (solved_b | dead_b | !changed_b);
        while service != 0 {
            let l = service.trailing_zeros() as usize;
            service &= service - 1;
            let bit = 1u64 << l;
            let verdict = self.slots[l].job.service(
                &mut self.boards,
                l,
                solved_b & bit != 0,
                dead_b & bit != 0,
            );
            if let Some(v) = verdict {
                self.active[l] = false;
                self.advance(l, v);
            }
        }
    }

    /// Resume the slot's lane coroutine with a verdict: it runs to its next
    /// request (a fresh job is loaded, the slot reactivates) or completes (seed
    /// supply drained — the slot stays idle and is never serviced again).
    fn advance(&mut self, l: usize, verdict: J::Verdict) {
        match Pin::new(&mut self.slots[l].lane).resume(verdict) {
            CoroutineState::Yielded(req) => {
                self.slots[l].job.load(&mut self.boards, l, &req);
                self.active[l] = true;
            }
            CoroutineState::Complete(()) => {}
        }
    }

    /// The retired-attempt tallies (yield/attempt counts for the summary line).
    pub fn stats(&self) -> Stats {
        self.shared.borrow().stats
    }
}

impl<I: Iterator<Item = u64>> GateStream<I> {
    /// Race `seeds` through the warp, one puzzle per seed (each retried until it
    /// yields).
    pub fn new(seeds: I, spec: &Spec) -> Self {
        Self::new_opts(seeds, spec, true)
    }

    /// [`new`](Self::new) with the warp's cross-stall subset-ladder memo toggleable
    /// (exact/first-fire-preserving, so both settings produce identical puzzles; only
    /// the cost differs).
    pub fn new_opts(seeds: I, spec: &Spec, ladder_memo: bool) -> Self {
        PuzzleStream::assemble(
            seeds,
            || GateJob::new(spec, ladder_memo),
            |shared| lane_co(shared.clone(), spec.clone()),
        )
    }
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
        let mut st: StripState = StripState::new(&solution);
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
