//! Experiment: the DFS prober's work as a function of its *propagation toolbox*.
//!
//! The production prober ([`super::search`] / [`super::propagate`]) propagates a fixed
//! toolbox before each branch: naked singles + the fused per-band **row/box**
//! hidden-single sweep. Columns straddle the row-major bands, so they are never swept —
//! a column hidden single is only ever resolved by branching. This module makes the
//! toolbox a knob so we can measure how the prober's work moves as we add or drop each
//! technique, holding the branch rule fixed at [`Bivalue`] (the production rule).
//!
//! The point is the warp/off-warp decision. The completion count — and therefore the
//! keep/revert *verdict* and the whole strip trajectory — is invariant to the toolbox
//! (completeness comes from the branch; propagation is pure pruning). So the *same*
//! stream of probes is posed no matter what propagation runs; only the work to settle
//! each probe changes. That lets us replay one trajectory through every toolbox and read
//! off the work each would have paid on the identical probe — the number we cannot get by
//! timing warp kernels in isolation.
//!
//! Toolbox bits (a [`Tb`]): `0x01` naked, `0x02` hidden-col, `0x04` hidden-row+box,
//! `0x08` LC-row (box↔row locked candidates), `0x10` LC-col (box↔column). LC is split by
//! orientation for the same reason hidden singles are: box↔row is in-lane per band,
//! box↔column straddles the bands. `0x00` is no propagation. The two production *probers*
//! sit at `0x05` (scalar: naked + row/box) and `0x07` (simt: naked + col + row/box, the
//! `warp_pass_full` kernel). Neither runs LC — LC is the *baseline* solver's technique
//! (off-warp, on stall); the LC bits model adding it to the prober's per-pass propagation.
//!
//! CONFLUENCE CAVEAT: naked + the full hidden-single set reaches a unique fixpoint
//! regardless of order, so the non-LC toolboxes' numbers are order-independent toolbox
//! properties. **LC without the full hidden-single set is not confluent** — the fixpoint
//! depends on how LC eliminations and naked placements interleave — so for an LC toolbox
//! that lacks any hidden direction the figures below are *this kernel order's* realization
//! (LC sweeps, then the single phase, per pass), not a canonical toolbox property. The
//! only fully-confluent LC point is the full toolbox `0x1F` (naked + all hidden + both LC).

use super::propagate::{Fixpoint, lone};
use crate::repr::banded::{Band, Banding, Bands, RowMajor};
use crate::repr::{Branchable, Digit, GridMask, Marks, SolverState};
use crate::scan::sieve::Sieve;
use crate::scan::{Bivalue, BranchStrategy, Scan};
use crate::solve::simt::{scalar_lc_col_sweep, scalar_lc_row_sweep};

type RM = Bands<RowMajor>;

/// The number of toolbox combinations: five independent technique bits (naked, hidden-col,
/// hidden-row+box, LC-row, LC-col).
pub const TOOLBOXES: usize = 32;

/// A toolbox: which techniques the per-pass propagation runs. Runtime flags rather than
/// const generics — we count nodes/passes, not time, so the per-pass flag tests do not
/// perturb the measurement, and 32 combinations stay one loop instead of a 32-way macro.
#[derive(Clone, Copy)]
pub struct Tb {
    pub nk: bool,  // naked single
    pub col: bool, // hidden single, column (cross-band fold)
    pub rb: bool,  // hidden single, row + box (in-lane per band)
    pub lcr: bool, // locked candidates, box <-> row (in-lane per band)
    pub lcc: bool, // locked candidates, box <-> column (cross-band)
}

impl Tb {
    /// Decode a toolbox index (bit0 naked, bit1 col, bit2 row+box, bit3 LC-row, bit4 LC-col).
    pub fn from_idx(t: usize) -> Tb {
        Tb {
            nk: t & 0x01 != 0,
            col: t & 0x02 != 0,
            rb: t & 0x04 != 0,
            lcr: t & 0x08 != 0,
            lcc: t & 0x10 != 0,
        }
    }
}

/// Index of the scalar off-warp prober's toolbox (naked + row/box).
pub const SCALAR: usize = 0x05;
/// Index of the SIMT on-warp prober's toolbox (naked + col + row/box).
pub const SIMT: usize = 0x07;

/// Human-readable name for a toolbox index, composed from its technique bits, with the
/// two production probers tagged.
pub fn tb_name(t: usize) -> String {
    let mut p: Vec<&str> = Vec::new();
    if t & 0x01 != 0 {
        p.push("N");
    }
    if t & 0x02 != 0 {
        p.push("C");
    }
    if t & 0x04 != 0 {
        p.push("RB");
    }
    if t & 0x08 != 0 {
        p.push("LCr");
    }
    if t & 0x10 != 0 {
        p.push("LCc");
    }
    let mut s = if p.is_empty() { "none".to_string() } else { p.join("+") };
    match t {
        SCALAR => s.push_str(" (scalar)"),
        SIMT => s.push_str(" (simt)"),
        _ => {}
    }
    s
}

/// The cells where `digit` is a **row or box** hidden single, detected (not placed) off
/// the frozen board — `lone` over each band's three lines and three boxes, the in-lane
/// units of the row-major view. Returns a placement mask so one pass can place every
/// hidden single it found at once.
#[inline]
fn rowbox_hidden_cells(state: &SolverState<RM>, digit: Digit) -> RM {
    let masked = state.candidates()[digit] & state.unsolved();
    let mut cells = RM::EMPTY;
    for b in 0..3 {
        let band = masked.band(b);
        for line in 0..3 {
            if let Some(slot) = lone(band.line(line)) {
                cells |= RM::cell(RowMajor::cell_at(b, Band::line_pos(line, slot)));
            }
        }
        for k in 0..3 {
            if let Some(slot) = lone(band.box_unit(k)) {
                cells |= RM::cell(RowMajor::cell_at(b, Band::box_pos(k, slot)));
            }
        }
    }
    cells
}

/// The cells where `digit` is a **column** hidden single — the technique the scalar
/// prober omits because a column's nine candidate bits straddle the three bands (bit `cc`
/// of each of the three 9-bit row runs, per band). The SIMT prober gets it for free via
/// `warp_pass_full`'s gather-free column fold. Detect-only, like [`rowbox_hidden_cells`].
#[inline]
fn col_hidden_cells(state: &SolverState<RM>, digit: Digit) -> RM {
    let masked = state.candidates()[digit] & state.unsolved();
    let mut cells = RM::EMPTY;
    for cc in 0..9 {
        // Column `cc`: row r = 3*b + l contributes bit `cc` of band b's line l.
        let mut col = 0usize;
        for b in 0..3 {
            let band = masked.band(b);
            for l in 0..3 {
                col |= ((band.line(l) >> cc) & 1) << (3 * b + l);
            }
        }
        if let Some(r) = lone(col) {
            let (b, l) = (r / 3, r % 3);
            cells |= RM::cell(RowMajor::cell_at(b, Band::line_pos(l, cc)));
        }
    }
    cells
}

/// Apply one locked-candidates `sweep` (a [`scalar_lc_row_sweep`] / [`scalar_lc_col_sweep`])
/// to `state`. LC only removes candidates (no placement), so it feeds the single sweeps
/// that follow in the same pass. Reads the row bands out, runs the one-orientation sweep,
/// and applies the per-`(cell, digit)` diff with [`forbid`](SolverState::forbid),
/// unsolved-gated so stale bits on decided cells are ignored. Returns whether it
/// eliminated anything on a live cell.
#[inline]
fn apply_lc(
    state: &mut SolverState<RM>,
    sweep: impl Fn(&mut [[u32; 3]; 9], &[u32; 3]) -> bool,
) -> bool {
    let mut sr: [[u32; 3]; 9] = core::array::from_fn(|d| {
        let l = state.candidates()[Digit::from_index(d)].to_lanes();
        [l[0], l[1], l[2]]
    });
    let sul = state.unsolved().to_lanes();
    let su = [sul[0], sul[1], sul[2]];
    let old = sr;
    if !sweep(&mut sr, &su) {
        return false;
    }
    let mut changed = false;
    for d in 0..9 {
        let digit = Digit::from_index(d);
        for b in 0..3 {
            let mut removed = old[d][b] & !sr[d][b] & su[b];
            while removed != 0 {
                let bit = removed.trailing_zeros() as usize;
                removed &= removed - 1;
                state.forbid(RowMajor::cell_at(b, bit), digit);
                changed = true;
            }
        }
    }
    changed
}

/// One pass under toolbox `tb`, structured to mirror **one `warp_pass_full` tick** so a
/// pass count is a faithful warp-tick proxy: enabled LC eliminations first, then the
/// naked-single set is frozen from the sieve and per digit the naked + enabled-hidden
/// cells are detected on the progressively-mutated board and placed in one wave (the
/// kernel's per-digit detect-mutate order). Returns the verdict and whether anything
/// changed; the dead/solved check is start-of-(single)-phase, so a placement collision is
/// read by the next pass — matching the kernel's naked-dead, with `smear_v`'s same-pass
/// collision catch reproduced below.
#[inline]
fn warp_pass(state: &mut SolverState<RM>, tb: Tb) -> (Fixpoint, bool) {
    let mut changed = false;
    if tb.lcr {
        changed |= apply_lc(state, scalar_lc_row_sweep);
    }
    if tb.lcc {
        changed |= apply_lc(state, scalar_lc_col_sweep);
    }
    let sieve = Sieve::<RM, 2>::compute_raw(state.candidates());
    let unsolved = state.unsolved();
    if (unsolved & sieve.dead()).any() {
        return (Fixpoint::Dead, false);
    }
    if !unsolved.any() {
        return (Fixpoint::Solved, false);
    }
    let naked = if tb.nk { unsolved & sieve.exactly(1) } else { RM::EMPTY };
    let mut dead = false;
    for di in 0..9 {
        let digit = Digit::from_index(di);
        let mut group = if tb.nk { naked & state.candidates()[digit] } else { RM::EMPTY };
        if tb.rb {
            group |= rowbox_hidden_cells(state, digit);
        }
        if tb.col {
            group |= col_hidden_cells(state, digit);
        }
        if group.any() {
            // The kernel's `smear_v` collision catch: a group whose cells share a unit
            // would place `digit` twice — read off the accumulated peer mask (a group
            // cell lands in another's peers iff they collide), so this pass is dead.
            let mut peers = RM::EMPTY;
            let mut rest = group;
            while rest.any() {
                let c = rest.first();
                rest &= !RM::cell(c);
                peers |= RM::peers(c);
            }
            dead |= (group & peers).any();
            state.place_group_with(digit, group, peers);
            changed = true;
        }
    }
    if dead { (Fixpoint::Dead, changed) } else { (Fixpoint::Stuck, changed) }
}

/// Drive `state` to a propagation fixpoint under toolbox `tb`, counting each [`warp_pass`]
/// into `passes` (the warp-tick proxy — with util at 100% the warp's wall time is
/// proportional to total passes). For the confluent toolboxes the reached fixpoint, and
/// thus the node count, is a well-defined function of the toolbox; see the module
/// confluence caveat for the LC-without-hidden cases.
#[inline]
fn propagate_tb(state: &mut SolverState<RM>, tb: Tb, passes: &mut u64) -> Fixpoint {
    loop {
        *passes += 1;
        match warp_pass(state, tb) {
            (Fixpoint::Stuck, true) => continue, // changed something — another pass may fire
            (Fixpoint::Stuck, false) => return Fixpoint::Stuck, // fixpoint
            (terminal, _) => return terminal,
        }
    }
}

/// Count completions of `state` up to `cap` under toolbox `tb`, branching by [`Bivalue`]
/// (the production prober's rule). Tallies every search node into `nodes` and every
/// propagation pass into `passes`. Returns `false` if the `cap_nodes` budget was exceeded
/// (`cap_nodes == 0` means unbounded). The propagation-poor toolboxes can blow up proving
/// a *keep* (an unsatisfiable restricted board must be exhausted), so the budget keeps the
/// experiment finite; a bailed search's counts are lower bounds.
fn count_tb(
    state: &mut SolverState<RM>,
    tb: Tb,
    cap: usize,
    found: &mut usize,
    nodes: &mut u64,
    passes: &mut u64,
    cap_nodes: u64,
) -> bool {
    *nodes += 1;
    if cap_nodes != 0 && *nodes >= cap_nodes {
        return false;
    }
    loop {
        match propagate_tb(state, tb, passes) {
            Fixpoint::Solved => {
                *found += 1;
                return true;
            }
            Fixpoint::Dead => return true,
            Fixpoint::Stuck => {}
        }
        let Scan::Branch { cell, candidates } = Bivalue::scan(state.candidates(), state.unsolved())
        else {
            return true;
        };
        let mut m = candidates;
        loop {
            let d = Digit::from_index(m.trailing_zeros() as usize);
            m &= m - 1;
            if m == 0 {
                state.place(cell, d);
                break;
            }
            let mut child = state.clone();
            child.place(cell, d);
            if !count_tb(&mut child, tb, cap, found, nodes, passes, cap_nodes) {
                return false;
            }
            if *found >= cap {
                return true;
            }
        }
    }
}

/// One toolbox's measurement of a single probe.
#[derive(Clone, Copy)]
pub struct TbResult {
    /// Search nodes (branch-tree size; capped at `cap_nodes` when `capped`).
    pub nodes: u64,
    /// Propagation passes summed over all nodes — the warp-tick proxy (one [`warp_pass`]
    /// == one `warp_pass_full` tick). With util at 100%, warp wall time is proportional to
    /// this; it is the multiplier the per-pass kernel cost rides on. (Not meaningful for
    /// LC, which runs off-warp in production.)
    pub passes: u64,
    /// The existence verdict (`true` = a completion exists). Meaningless when `capped`.
    pub exists: bool,
    /// Whether the node budget was hit before the search finished.
    pub capped: bool,
}

/// Run the existence probe (`cap = 1`, the uniqueness gate's `has_completion`) on `probe`
/// under every toolbox selected in `which` (bit `t` set => run toolbox `t`), each on its
/// own clone. `probe` must already carry the gate's restriction (the stripped clue's digit
/// forbidden). The non-capped verdicts all agree (toolbox-invariant completeness); the
/// caller can assert that and use any one as ground truth.
pub fn probe_all(probe: &SolverState<RM>, which: u32, cap_nodes: u64) -> [TbResult; TOOLBOXES] {
    let mut out = [TbResult { nodes: 0, passes: 0, exists: false, capped: false }; TOOLBOXES];
    for t in 0..TOOLBOXES {
        if which & (1 << t) == 0 {
            continue;
        }
        let mut st = probe.clone();
        let (mut found, mut n, mut p) = (0usize, 0u64, 0u64);
        let ok = count_tb(&mut st, Tb::from_idx(t), 1, &mut found, &mut n, &mut p, cap_nodes);
        out[t] = TbResult {
            nodes: if cap_nodes != 0 { n.min(cap_nodes) } else { n },
            passes: p,
            exists: found >= 1,
            capped: !ok,
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repr::DigitGrid;

    const PUZZLE: &str = "\
        53..7....\
        6..195...\
        .98....6.\
        8...6...3\
        4..8.3..1\
        7...2...6\
        .6....28.\
        ...419..5\
        ....8..79";

    fn state(s: &str) -> SolverState<RM> {
        SolverState::from_digits(&DigitGrid::parse(s).unwrap())
    }

    /// Every toolbox must agree on the existence verdict — every technique is pure
    /// pruning, so none can change whether a completion exists, only the work to find out.
    /// This is the soundness anchor for the new LC-row / LC-col sweeps and the no-singles
    /// combinations.
    #[test]
    fn all_toolboxes_agree_on_verdict() {
        for s in [PUZZLE, &".".repeat(81)] {
            let r = probe_all(&state(s), u32::MAX, 50_000_000);
            let truth = r[TOOLBOXES - 1].exists;
            for (t, res) in r.iter().enumerate() {
                assert!(!res.capped, "toolbox {t} capped on a tractable board");
                assert_eq!(res.exists, truth, "toolbox {t} disagrees on verdict");
            }
        }
    }

    /// Adding a *singles* technique does not raise the node count here: a placement
    /// shrinks the board (one fewer unsolved cell) without re-targeting the Bivalue branch
    /// for the worse. This is an empirical regularity, NOT a theorem — node count is not
    /// generally monotone under adding an inference rule, because the rule changes the
    /// candidate state the branch heuristic reads.
    ///
    /// It is deliberately NOT asserted for LC: LC *eliminates candidates without placing*,
    /// and {naked, LC} without the full hidden-single set is non-confluent, so `+LC` can
    /// change the fixpoint and the node count either way.
    #[test]
    fn more_singles_never_cost_more_nodes() {
        let r = probe_all(&state(PUZZLE), u32::MAX, 0);
        assert!(r[SIMT].nodes <= r[SCALAR].nodes); // +column
        assert!(r[SCALAR].nodes <= r[0x01].nodes); // +row/box
        assert!(r[0x01].nodes <= r[0x00].nodes); // +naked
    }
}
