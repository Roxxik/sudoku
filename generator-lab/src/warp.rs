//! The attempt **warp**: K independent strip attempts driven in lockstep, with
//! per-lane refill. This is the orchestration that "packs multiple attempts in
//! parallel"; the uniqueness gate it batches is resolved by the packed-DFS prober
//! (`crate::packed`, the gather-free W=8 smear+ALU kernel).
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
//!     probe is handed to the packed prober ([`crate::packed`]), which runs it on
//!     one of its 8 SIMD slots alongside seven other lanes' probes. (The baseline
//!     gate is still scalar per-lane — the remaining Amdahl half.)
//!
//! **Streaming, not a barrier.** The packed prober owns the loop ([`run_warp`]
//! drives [`crate::packed::PackedProber::run_stream`]): each SIMD slot streams one
//! logical lane's gates, and the moment a slot reaches a verdict the refill
//! callback resolves it ([`resolve_gate_with`]: revert/keep + baseline), walks that
//! lane to its next gate, and hands the new probe straight back — no two-phase
//! advance-all-then-resolve-all barrier, no FIFO-depth knob. A lane that exhausts
//! its quota finalizes (verify) and its slot grabs the next unstarted lane, so at
//! most 8 attempts are ever in flight yet the warp stays full while work remains.
//! Logical lanes are independent, so the 8-slot interleave can't change any lane's
//! outcome (`tests/equiv.rs` pins each lane byte-identical to its sequential run).

use crate::bb::{BitBoard, Placed};
use crate::generator::{board_from_cells, random_full_grid};
use crate::grid::{Board, CELLS, Digit, digit_to_bit};
use crate::packed::{LANES, PackedProber, Probe};
use crate::rng::Rng;
use crate::spec::Spec;
use crate::verify::verify;

const FNV_PRIME: u64 = 0x100000001b3;
const FNV_OFFSET: u64 = 0xcbf29ce484222325;

/// Per-lane / aggregate tallies. Mirrors generator-lab's `GenStats` field-for-
/// field so a lane's totals can be compared against the sequential run.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub attempts: usize,
    pub successes: usize,
    pub never_fired: usize,
    pub not_forced: usize,
    pub total_givens: usize,
}

impl Stats {
    fn add(&mut self, o: &Stats) {
        self.attempts += o.attempts;
        self.successes += o.successes;
        self.never_fired += o.never_fired;
        self.not_forced += o.not_forced;
        self.total_givens += o.total_givens;
    }
}

/// One lane = one in-flight attempt plus the running tallies/fingerprint of every
/// attempt this lane has retired. State mirrors generator-lab's `attempt` locals,
/// kept across slot refills so the attempt can be paused at its uniqueness gate.
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
    bb: BitBoard,
    placed: Placed,
    cells: [Digit; CELLS],
    positions: [usize; CELLS],
    pos_idx: usize,
    best: Option<[Digit; CELLS]>,
    req_met: bool,

    // --- retired-attempt accounting ---
    stats: Stats,
    fp: u64,
}

impl Lane {
    fn new(rng: Rng, quota: usize) -> Self {
        // Placeholder bitboard; replaced by `start_attempt` before any use.
        let empty = Board::empty();
        Lane {
            rng,
            remaining: quota,
            active: false,
            pending: None,
            bb: BitBoard::from_board(&empty),
            placed: Placed::from_board(&empty),
            cells: [0; CELLS],
            positions: core::array::from_fn(|i| i),
            pos_idx: 0,
            best: None,
            req_met: false,
            stats: Stats::default(),
            fp: FNV_OFFSET,
        }
    }

    /// Begin a fresh attempt: random full grid, shuffled strip order, reset gate
    /// state. Consumes RNG identically to generator-lab's `attempt` (full grid
    /// fill, then the strip-order shuffle) so the stream stays faithful.
    fn start_attempt(&mut self) {
        let solution = random_full_grid(&mut self.rng);
        self.cells = core::array::from_fn(|i| solution.cell(i));
        self.bb = BitBoard::from_board(&solution);
        self.placed = Placed::from_board(&solution);
        self.positions = core::array::from_fn(|i| i);
        self.rng.shuffle(&mut self.positions);
        self.pos_idx = 0;
        self.best = None;
        self.req_met = false;
        self.active = true;
        self.pending = None;
        self.remaining -= 1;
        self.stats.attempts += 1;
    }

    /// Walk the cheap part of the strip until the lane either reaches a
    /// uniqueness-gate decision (sets `pending`) or exhausts the attempt — in
    /// which case it finalizes and refills (looping) until either a gate is
    /// reached or its quota runs out (then it goes idle).
    fn advance_to_gate(&mut self, spec: &Spec) {
        loop {
            if self.pos_idx >= CELLS {
                self.finalize(spec);
                self.active = false;
                if self.remaining > 0 {
                    self.start_attempt();
                    continue;
                }
                return;
            }
            let i = self.positions[self.pos_idx];
            self.pos_idx += 1;
            if self.cells[i] == 0 {
                continue; // already stripped
            }
            let orig = self.cells[i];
            self.cells[i] = 0;
            let cand = self.bb.apply_clear(i, orig, &mut self.placed);
            let alts = cand & !digit_to_bit(orig);
            if alts == 0 {
                // Still a naked single: strip is trivially valid, verdict
                // unchanged — both gates skippable, just carry `req_met`.
                if self.req_met {
                    self.best = Some(self.cells);
                }
                continue;
            }
            // Reached the batch point.
            self.pending = Some((i, orig, alts));
            return;
        }
    }

    /// Revert the speculative clear of `cell` (held `orig`): the strip is rejected.
    fn revert(&mut self, cell: usize, orig: Digit) {
        self.cells[cell] = orig;
        self.bb.apply_place(cell, orig, &mut self.placed);
    }

    /// Finalize the current attempt: verify `best`, tally the outcome, fold the
    /// fingerprint — byte-identical to generator-lab's `run_attempts` per-outcome
    /// bookkeeping.
    fn finalize(&mut self, spec: &Spec) {
        match self.best {
            Some(snap) => {
                let seed = board_from_cells(&snap);
                if verify(&seed, spec) {
                    self.stats.successes += 1;
                    self.stats.total_givens += seed.givens();
                    for i in 0..CELLS {
                        self.fp ^= snap[i] as u64;
                        self.fp = self.fp.wrapping_mul(FNV_PRIME);
                    }
                } else {
                    self.stats.not_forced += 1;
                    self.fp = self.fp.wrapping_mul(FNV_PRIME);
                }
            }
            None => {
                self.stats.never_fired += 1;
                self.fp = self.fp.wrapping_mul(FNV_PRIME);
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

    if nonunique {
        lane.revert(i, orig);
        return;
    }
    // Baseline gate + requirement counts.
    let outcome = lane.bb.baseline(baseline, forced);
    if !outcome.solved {
        lane.revert(i, orig);
        return;
    }
    // Accept the strip.
    lane.req_met = spec.requirement_met(&outcome.counts);
    if lane.req_met {
        lane.best = Some(lane.cells);
    }
}

/// Snapshot a lane's pending gate as a [`Probe`] for the packed prober: the
/// board's row-major bands + empty mask, the stripped cell, and its alternates.
fn probe_of(lane: &Lane) -> Probe {
    let (cell, _orig, alts) = lane.pending.expect("probe_of on a lane with no gate");
    let (r, unsolved) = lane.bb.export_r();
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
/// (`tests/equiv.rs` pins this).
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
            ls[ll].advance_to_gate(spec);
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
            ls[ll].advance_to_gate(spec);
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
