//! The attempt **warp**: K independent strip attempts driven in lockstep, with
//! per-lane refill. This is the orchestration that "packs multiple attempts in
//! parallel"; the uniqueness gate it batches is resolved by the packed-DFS prober
//! ([`crate::simt::prober`], the gather-free W=8 smear+ALU kernel).
//!
//! ## The cheap/expensive split and on-demand refill
//!
//! A lane's attempt is generator-lab's `attempt` body, made *resumable*. The work
//! splits into a cheap part and an expensive part:
//!
//!   - **cheap (per-lane, scalar):** walk the shuffled strip order skipping
//!     already-stripped cells and the `alts == 0` fast path (a cell that is still
//!     a naked single — the strip is trivially valid), maintaining the incremental
//!     bitboard, until the lane hits a cell whose clear leaves alternatives
//!     (`alts != 0`). That cell needs the **uniqueness gate** — the lane pauses
//!     there with its `(cell, orig, alts)` pending. This is [`Lane::advance_to_gate`].
//!
//!   - **expensive (the gate):** the uniqueness prober + baseline. The pending
//!     probe is handed to the packed prober ([`crate::simt::prober`]), which runs it on
//!     one of its 8 SIMD slots alongside seven other lanes' probes. (The baseline
//!     gate is still scalar per-lane — the remaining Amdahl half.)
//!
//! **Streaming, not a barrier.** The packed prober owns the loop ([`run_warp`]
//! drives [`crate::simt::prober::PackedProber::run_stream`]): each SIMD slot streams one
//! logical lane's gates, and the moment a slot reaches a verdict the refill
//! callback resolves it ([`resolve_gate_with`]: revert/keep + baseline), walks that
//! lane to its next gate, and hands the new probe straight back — no two-phase
//! advance-all-then-resolve-all barrier, no FIFO-depth knob. A lane that exhausts
//! its quota finalizes (verify) and its slot grabs the next unstarted lane, so at
//! most 8 attempts are ever in flight yet the warp stays full while work remains.
//! Logical lanes are independent, so the 8-slot interleave can't change any lane's
//! outcome (`tests/equiv_warp.rs` pins each lane byte-identical to its sequential run).

use crate::generator::{GeneratedPuzzle, Stats, StripState, board_from_cells, random_full_grid};
use crate::grid::{Board, CELLS, Digit};
use crate::simt::prober::{LANES, PackedProber, Probe};
use crate::rng::Rng;
use crate::spec::Spec;
use crate::util::{FNV_OFFSET, FNV_PRIME, fnv_fold_cells};
use crate::verify::verify;

/// One lane = one in-flight attempt plus the running tallies/fingerprint of every
/// attempt this lane has retired. The board state and per-cell gate logic are the
/// shared [`StripState`]; the lane adds the resumable walk (strip order + cursor)
/// and the retired-attempt accounting, kept across slot refills so the attempt can
/// be paused at its uniqueness gate.
struct Lane {
    rng: Rng,
    /// Attempts still owed by this lane (its share of the fixed work budget).
    remaining: usize,
    /// True while an attempt is in flight (between start and finalize).
    active: bool,
    /// Set when the lane is paused at a uniqueness-gate decision: `(cell, orig,
    /// alts)`. `None` means "needs advancing" (or idle).
    pending: Option<(usize, Digit, u16)>,

    // --- in-flight attempt state (valid iff `active`) ---
    /// The board being stripped + the shared gate logic (the same `StripState` the
    /// sequential `attempt` drives).
    strip: StripState,
    /// The full solution grid this attempt is stripping (cells before any strip),
    /// kept so a success can report its solution like the scalar `generate`.
    solution: [Digit; CELLS],
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
            remaining: quota,
            active: false,
            pending: None,
            // Placeholder state; replaced by `start_attempt` before any use.
            strip: StripState::new(&Board::empty()),
            solution: [0; CELLS],
            positions: core::array::from_fn(|i| i),
            pos_idx: 0,
            stats: Stats::default(),
            fp: FNV_OFFSET,
        }
    }

    /// Begin a fresh attempt: random full grid, shuffled strip order, reset gate
    /// state. Consumes RNG identically to generator-lab's `attempt` (full grid
    /// fill, then the strip-order shuffle) so the stream stays faithful.
    fn start_attempt(&mut self) {
        let solution = random_full_grid(&mut self.rng);
        self.solution = core::array::from_fn(|i| solution.cell(i));
        self.strip = StripState::new(&solution);
        self.positions = core::array::from_fn(|i| i);
        self.rng.shuffle(&mut self.positions);
        self.pos_idx = 0;
        self.active = true;
        self.pending = None;
        self.remaining -= 1;
        self.stats.attempts += 1;
    }

    /// Walk the cheap part of the strip until the lane either reaches a
    /// uniqueness-gate decision (sets `pending`) or exhausts the attempt — in
    /// which case it finalizes and refills (looping) until either a gate is
    /// reached or its quota runs out (then it goes idle).
    fn advance_to_gate<F: FnMut(GeneratedPuzzle)>(&mut self, spec: &Spec, on_found: &mut F) {
        loop {
            if self.pos_idx >= CELLS {
                if let Some(p) = self.finalize(spec) {
                    on_found(p);
                }
                self.active = false;
                if self.remaining > 0 {
                    self.start_attempt();
                    continue;
                }
                return;
            }
            let i = self.positions[self.pos_idx];
            self.pos_idx += 1;
            if self.strip.cells[i] == 0 {
                continue; // already stripped
            }
            let orig = self.strip.cells[i];
            let alts = self.strip.strip(i, orig);
            if alts == 0 {
                self.strip.keep_trivial(spec);
                continue;
            }
            // Reached the batch point.
            self.pending = Some((i, orig, alts));
            return;
        }
    }

    /// Finalize the current attempt: verify `best`, tally the outcome, fold the
    /// fingerprint — byte-identical to generator-lab's `run_attempts` per-outcome
    /// bookkeeping.
    fn finalize(&mut self, spec: &Spec) -> Option<GeneratedPuzzle> {
        match self.strip.best {
            Some(snap) => {
                let seed = board_from_cells(&snap);
                if verify(&seed, spec) {
                    self.stats.successes += 1;
                    let givens = seed.givens();
                    self.stats.total_givens += givens;
                    fnv_fold_cells(&mut self.fp, &snap);
                    Some(GeneratedPuzzle { puzzle: seed, solution: board_from_cells(&self.solution), givens })
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

/// Finish resolving one lane's pending gate given the already-decided uniqueness
/// verdict `nonunique` (from the packed prober, or the scalar fallback). Mirrors
/// the scalar gate sequence: if non-unique, revert; else baseline solvability +
/// requirement counts; accept or revert.
fn resolve_gate_with(lane: &mut Lane, spec: &Spec, baseline: u32, forced: u32, nonunique: bool) {
    let (i, orig, _alts) = lane.pending.take().expect("resolve_gate on a lane with no pending gate");
    lane.strip.resolve_gate(i, orig, nonunique, spec, baseline, forced);
}

/// Snapshot a lane's pending gate as a [`Probe`] for the packed prober: the
/// board's row-major bands + empty mask, the stripped cell, and its alternates.
fn probe_of(lane: &Lane) -> Probe {
    let (cell, _orig, alts) = lane.pending.expect("probe_of on a lane with no gate");
    let (r, unsolved) = lane.strip.bb.export_r();
    Probe { r, unsolved, cell, alts }
}

/// Result of a warp run: aggregate tallies plus the per-lane `(stats, fp)` pairs,
/// the latter for the equivalence cross-check against the sequential generator.
pub struct WarpResult {
    pub stats: Stats,
    pub per_lane: Vec<(Stats, u64)>,
}

/// Run `lanes` independent attempt-streams for `spec`, each doing exactly
/// `attempts_per_lane` attempts from its own seed `base_seed + lane_index`. Total
/// attempts = `lanes * attempts_per_lane`. Fixed-work, deterministic.
///
/// **Streaming refill (no macro-step barrier).** The packed prober owns the loop:
/// its 8 SIMD slots each run one logical lane's current uniqueness gate, and when a
/// slot reaches a verdict the refill callback *generates that lane's next gate on
/// demand* — apply the verdict (revert/keep + baseline), walk the strip to the next
/// gate (or finalize the attempt and start the lane's next one), and hand back the
/// new probe. A logical lane is bound to a slot until its whole attempt quota is
/// done, then the slot grabs the next unstarted lane. So at most 8 attempts are ever
/// in flight, yet the warp stays full — no FIFO-depth knob, no oversubscription.
/// Lanes are independent, so each logical lane's `(stats, fp)` is byte-identical to
/// the sequential run from its seed regardless of how the 8 slots interleave
/// (`tests/equiv_warp.rs` pins this).
pub fn run_warp(base_seed: u64, spec: &Spec, lanes: usize, attempts_per_lane: usize) -> WarpResult {
    let baseline = spec.baseline_mask();
    let forced = spec.forced_mask();

    let mut ls: Vec<Lane> = (0..lanes)
        .map(|l| Lane::new(Rng::from_seed(base_seed + l as u64), attempts_per_lane))
        .collect();
    let mut prober = PackedProber::new();
    // Which logical lane each SIMD slot is currently streaming, and the next
    // unstarted logical lane to hand out when a slot's lane finishes its quota.
    let mut slot_lane = [usize::MAX; LANES];
    let mut next_lane = 0usize;

    prober.run_stream(|slot, verdict| {
        // Continue the slot's current logical lane: apply the gate verdict, then
        // walk to its next gate. If it produces one, that's this slot's refill.
        if let Some(v) = verdict {
            let ll = slot_lane[slot];
            resolve_gate_with(&mut ls[ll], spec, baseline, forced, v);
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

/// Generate puzzles by racing the W=8 packed prober across independent seed streams
/// until `target` puzzles have been produced, then stop. SIMD slot `s` owns one
/// unbounded seed stream (`base_seed + s`) and the packed prober keeps all 8 slots
/// in flight, so this harvests the warp's full per-core throughput — the realistic
/// way to *use* the SIMT prober (batch generation of many puzzles). A single puzzle
/// from a single seed is inherently sequential; for that use `generator::generate`.
///
/// Deterministic for a given `base_seed`. Returns the puzzles (each spec-verified,
/// so safe to feed straight to core's verifier) and the aggregate attempt stats.
/// In-flight slots are drained once `target` is reached, so a few attempts past the
/// last find may run; the returned Vec is exactly `target` long (a single warp pass
/// can surface more than one success).
pub fn find_puzzles(base_seed: u64, spec: &Spec, target: usize) -> (Vec<GeneratedPuzzle>, Stats) {
    let baseline = spec.baseline_mask();
    let forced = spec.forced_mask();
    let mut found: Vec<GeneratedPuzzle> = Vec::with_capacity(target);
    if target == 0 {
        return (found, Stats::default());
    }

    // One unbounded seed stream per SIMD slot (1:1, no lane pool to refill from).
    let mut ls: Vec<Lane> =
        (0..LANES).map(|s| Lane::new(Rng::from_seed(base_seed + s as u64), usize::MAX)).collect();
    let mut prober = PackedProber::new();

    prober.run_stream(|slot, verdict| {
        if found.len() >= target {
            return None; // enough found: let the in-flight slots drain
        }
        let lane = &mut ls[slot];
        match verdict {
            Some(v) => resolve_gate_with(lane, spec, baseline, forced, v),
            None if !lane.active => lane.start_attempt(), // initial fill
            None => {}
        }
        lane.advance_to_gate(spec, &mut |p| found.push(p));
        if found.len() >= target {
            return None;
        }
        // Unbounded quota ⇒ advance always parks the lane at a fresh gate.
        if lane.pending.is_some() { Some(probe_of(lane)) } else { None }
    });

    found.truncate(target);
    let mut stats = Stats::default();
    for lane in &ls {
        stats.add(&lane.stats);
    }
    (found, stats)
}
