//! The bb-based strip scaffolding the native SIMT warp still rides on.
//!
//! This module has been trimmed to exactly what `crate::simt` needs: the per-attempt
//! mutable [`StripState`] (the shared gate logic the warp's per-lane state machine
//! embeds), [`GeneratedPuzzle`]/[`Stats`], and the `board_from_cells`/`random_full_grid`
//! primitives. The scalar sequential generator (`attempt`/`generate`/`run_attempts`/
//! `determinism_fp`) has moved to [`crate::generate`] on the new `repr` foundations —
//! that is the shipped scalar/wasm path now. Only the warp, which is not yet ported off
//! the `bb` bitboard, keeps this scaffolding alive.

use crate::bb::{BitBoard, Placed};
use crate::grid::{Board, CELLS, Digit, digit_to_bit};
use crate::spec::Spec;
use crate::technique_kinds::KindMask;

// The grid filler moved to [`crate::fill`]; re-export it here so callers that reach
// for the generation primitive via `generator::` (the warp host + gridbench) keep working.
pub use crate::fill::random_full_grid;

/// A generated puzzle and the full solution it was stripped from.
pub struct GeneratedPuzzle {
    pub puzzle: Board,
    pub solution: Board,
    pub givens: usize,
}

/// Reconstruct a `Board` (cells + naked candidates) from a bare puzzle grid
/// `cells` (present cell = its digit, `0` = stripped) — used to materialize a
/// candidate `best`/`seed` for [`verify`] and for the desync cross-check.
pub fn board_from_cells(cells: &[Digit; CELLS]) -> Board {
    let mut b = Board::empty();
    for (i, &d) in cells.iter().enumerate() {
        if d != 0 {
            b.place(i, d);
        }
    }
    b
}

/// The mutable board state of one strip attempt, and the per-cell gate logic that
/// drives it. Shared by the sequential [`attempt`] and the resumable per-lane
/// state machine of the warp (`crate::simt::host::Lane` embeds one), so the gate
/// sequence — `alts == 0` fast path, uniqueness gate, baseline gate,
/// `req_met`/`best` update — lives in ONE place and the two drivers cannot drift.
///
/// `bb` is the single source of candidate truth (it already holds every candidate
/// band). The only scalar shadow kept is the bare puzzle grid `cells` (present
/// cell = its digit, `0` = stripped) — the one thing `bb` can't derive: which
/// digit a surviving peer holds. `bb` is maintained incrementally (a clear/place
/// only touches cell `i` and its peers; `apply_clear` reads the reopened
/// candidates straight off `placed`), so there is no duplicate per-cell candidate
/// array and no per-position `from_board` rebuild.
pub(crate) struct StripState {
    pub bb: BitBoard,
    placed: Placed,
    pub cells: [Digit; CELLS],
    /// The bare-grid snapshot of the most-stripped requirement-meeting state; the
    /// candidate board is rebuilt from it only if the attempt succeeds.
    pub best: Option<[Digit; CELLS]>,
    /// The running requirement verdict of the current accepted board (see the
    /// `alts == 0` fast path). The full grid is trivially baseline-solvable but
    /// fires nothing, so it starts false.
    pub req_met: bool,
}

impl StripState {
    /// Fresh state stripping the full `solution` grid (nothing removed yet).
    pub fn new(solution: &Board) -> Self {
        StripState {
            bb: BitBoard::from_board(solution),
            placed: Placed::from_board(solution),
            cells: core::array::from_fn(|i| solution.cell(i)),
            best: None,
            req_met: false,
        }
    }

    /// Speculatively strip cell `i` (holding `orig`): clear it from the grid and
    /// the bitboard, returning the alternate digits the cell could still take —
    /// `0` means it is still a naked single, so the strip is trivially valid.
    pub fn strip(&mut self, i: usize, orig: Digit) -> u16 {
        self.cells[i] = 0;
        let cand = self.bb.apply_clear(i, orig, &mut self.placed);
        debug_assert!(
            self.bb == BitBoard::from_board(&board_from_cells(&self.cells)),
            "bb desync after clear at {i}"
        );
        cand & !digit_to_bit(orig)
    }

    /// `alts == 0` fast path: clearing the cell left only its own digit, so `i` is
    /// still a naked single. The baseline would re-place it immediately and reach a
    /// byte-identical closure — so the strip stays unique AND baseline-solvable and
    /// the requirement verdict is unchanged. Both gates are skippable; just carry
    /// `req_met` into `best`.
    pub fn keep_trivial(&mut self, spec: &Spec) {
        debug_assert!(
            {
                let o = self.bb.baseline(spec.baseline_mask(), spec.forced_mask());
                o.solved && spec.requirement_met(&o.counts) == self.req_met
            },
            "alts==0 fast-path invariant broke"
        );
        if self.req_met {
            self.best = Some(self.cells);
        }
    }

    /// Revert a rejected strip of `cell` (held `orig`).
    pub fn revert(&mut self, cell: usize, orig: Digit) {
        self.cells[cell] = orig;
        self.bb.apply_place(cell, orig, &mut self.placed);
        debug_assert!(
            self.bb == BitBoard::from_board(&board_from_cells(&self.cells)),
            "bb desync after revert at {cell}"
        );
    }

    /// Resolve the gates for the strip of `cell` (held `orig`) given the
    /// already-decided uniqueness verdict `nonunique`: if non-unique, revert; else
    /// run the baseline gate (`baseline`/`forced` masks) and either accept —
    /// keeping `cells`/`bb` in the stripped state and updating `req_met`/`best` —
    /// or revert. The warp supplies `nonunique` from the packed prober (`simt`); the
    /// gate logic lives here so the prober verdict and the baseline/`best` update
    /// stay in one place.
    pub fn resolve_gate(
        &mut self,
        cell: usize,
        orig: Digit,
        nonunique: bool,
        spec: &Spec,
        baseline: KindMask,
        forced: KindMask,
    ) {
        if nonunique {
            self.revert(cell, orig);
            return;
        }
        // Baseline gate + requirement counts (one tracked solve).
        let outcome = self.bb.baseline(baseline, forced);
        if !outcome.solved {
            self.revert(cell, orig);
            return;
        }
        self.req_met = spec.requirement_met(&outcome.counts);
        if self.req_met {
            self.best = Some(self.cells);
        }
    }
}

/// Per-run / per-lane tallies, for throughput/yield reporting and the warp's
/// per-lane equivalence cross-check against the sequential generator.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub attempts: usize,
    pub successes: usize,
    pub never_fired: usize,
    pub not_forced: usize,
    pub total_givens: usize,
}

impl Stats {
    /// Fold another tally into this one (the warp aggregates per-lane `Stats`).
    pub fn add(&mut self, o: &Stats) {
        self.attempts += o.attempts;
        self.successes += o.successes;
        self.never_fired += o.never_fired;
        self.not_forced += o.not_forced;
        self.total_givens += o.total_givens;
    }
}

