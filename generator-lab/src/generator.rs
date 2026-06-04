//! The spec-driven `random`-method generator — core's
//! `make_puzzle_for_spec_with_search` with the `random` path ONLY (no local
//! search, no construction) and none of the play-time bookkeeping.
//!
//! Per attempt: a random full grid, then strip cells in random order keeping a
//! strip iff the puzzle stays unique (prober) AND baseline-solvable (spec
//! toolbox). The most-stripped state whose baseline trace meets the requirement
//! counts is remembered as `best`; after the strip, if a `best` exists and it
//! passes [`verify`], the attempt succeeds. This is the exact gate sequence and
//! `best`/requirement/verify logic from core, just stripped to bools + counts.

use crate::bb::{BitBoard, Placed};
use crate::grid::{Board, CELLS, Digit, PEERS, digit_to_bit};
use crate::rng::Rng;
use crate::spec::Spec;
use crate::techniques::KindMask;
use crate::util::{FNV_OFFSET, FNV_PRIME, fnv_fold_cells};
use crate::verify::verify;

/// A random complete solution grid. Same MRV+shuffle search as core — identical
/// grid and RNG stream for a given seed — but the search state is a
/// *digit-transposed bitboard* (solver-lab's ARM-winning `bitboard` rep) instead
/// of a per-cell candidate `Board`. The fill is scan-bound (~83 nodes/grid, one
/// MRV scan each, ~1.7 backtracks), and this rep makes both the scan and the
/// placement cheaper while staying popcount-free — the ARM win — and roughly
/// halves the fill on native (measured in `examples/gridbench.rs`).
#[cfg_attr(feature = "profiling", inline(never))]
pub fn random_full_grid(rng: &mut Rng) -> Board {
    let mut f = Fill {
        board: [ALL_CELLS; 9],
        unsolved: ALL_CELLS,
        cells: [0; CELLS],
    };
    let ok = f.fill(rng);
    debug_assert!(ok, "fill should always succeed on empty board");
    Board::from_solved_cells(f.cells)
}

/// `PEER_MASK[c]` has a bit set for each of cell `c`'s 20 peers (self excluded),
/// as an 81-bit value in a `u128`. A placement AND-NOTs the placed digit's board
/// with it to forbid that digit on every peer in a single op — no 20-peer walk.
const PEER_MASK: [u128; CELLS] = {
    let mut m = [0u128; CELLS];
    let mut i = 0;
    while i < CELLS {
        let mut k = 0;
        while k < 20 {
            m[i] |= 1u128 << PEERS[i][k];
            k += 1;
        }
        i += 1;
    }
    m
};

/// All 81 cell bits set (cells 0..81), the rest of the `u128` zero.
const ALL_CELLS: u128 = (1u128 << CELLS) - 1;

/// Digit-transposed fill state: `board[d]` bit `c` is set iff digit `d+1` can
/// still go at cell `c`; `unsolved` masks the cells not yet decided. A decided
/// cell's stale bits in the other boards are never cleared — they are gated out
/// by `unsolved` everywhere the scan reads them, so candidates of *unsolved*
/// cells stay exactly correct (= the `Board`'s naked candidates). `cells` records
/// the placed digit at each cell, the only thing the bitboard can't answer, for
/// the final grid.
struct Fill {
    board: [u128; 9],
    unsolved: u128,
    cells: [Digit; CELLS],
}

impl Fill {
    /// MRV scan: the unsolved cell with the fewest candidates (ties → lowest
    /// index), popcount-free, returning `(cell, count, candidate_mask)` with
    /// `count == 0` => a dead unsolved cell and `cell == usize::MAX` => solved.
    ///
    /// The symmetric sieve builds `ones..` = "unsolved cells with at least k
    /// candidates"; the lowest non-empty exactly-`k` tier (`tier_k & !tier_{k+1}`)
    /// is the minimum count, and its lowest set bit is the lowest-index cell
    /// achieving it — byte-identical to the scalar fill's `n < bn` pick.
    ///
    /// Only the first FOUR levels are computed eagerly: 82.8% of fill scans have
    /// a min tier ≤ 3 (measured: 1:41.8%, 2:27.8%, 3:13.2%), and the capped sieve
    /// is 7 `u128` ops/digit vs the full 17. The ~16% of nodes whose every
    /// unsolved cell has ≥ 4 candidates (the early, near-empty board) fall back to
    /// the full sieve for tiers 4..9.
    #[inline]
    fn scan(&self) -> (usize, u32, u16) {
        let u = self.unsolved;
        let (mut ones, mut twos, mut threes, mut fours) = (0u128, 0u128, 0u128, 0u128);
        for d in 0..9 {
            let b = self.board[d] & u;
            fours |= threes & b;
            threes |= twos & b;
            twos |= ones & b;
            ones |= b;
        }
        if u & !ones != 0 {
            return (usize::MAX, 0, 0); // some unsolved cell has no candidate
        }
        if u == 0 {
            return (usize::MAX, 10, 0); // every cell decided — solved
        }
        let t1 = ones & !twos;
        if t1 != 0 {
            return self.pick(t1, 1);
        }
        let t2 = twos & !threes;
        if t2 != 0 {
            return self.pick(t2, 2);
        }
        let t3 = threes & !fours;
        if t3 != 0 {
            return self.pick(t3, 3);
        }
        // Rare: every unsolved cell has ≥ 4 candidates. Full sieve for tiers 4..9.
        let mut a = [0u128; 11]; // a[1..=9] used; a[10] stays 0 as the k==9 upper
        for d in 0..9 {
            let b = self.board[d] & u;
            let mut k = 9;
            while k >= 2 {
                a[k] |= a[k - 1] & b;
                k -= 1;
            }
            a[1] |= b;
        }
        for k in 4..=9usize {
            let tier = a[k] & !a[k + 1];
            if tier != 0 {
                return self.pick(tier, k as u32);
            }
        }
        unreachable!("unsolved non-empty but no tier matched")
    }

    /// Materialize the scan's result for the chosen `cell` (lowest set bit of
    /// `tier`): its candidate mask, gathered from the nine boards.
    #[inline]
    fn pick(&self, tier: u128, count: u32) -> (usize, u32, u16) {
        let cell = tier.trailing_zeros() as usize;
        let cb = 1u128 << cell;
        let mut mask = 0u16;
        for d in 0..9 {
            if self.board[d] & cb != 0 {
                mask |= 1 << d;
            }
        }
        (cell, count, mask)
    }

    /// Decide cell `cell` as digit `d`: drop it from `unsolved` and forbid `d` on
    /// every peer — two `u128` AND-NOTs, regardless of peer count.
    #[inline]
    fn place(&mut self, cell: usize, d: Digit) {
        self.unsolved &= !(1u128 << cell);
        self.board[(d - 1) as usize] &= !PEER_MASK[cell];
        self.cells[cell] = d;
    }

    fn fill(&mut self, rng: &mut Rng) -> bool {
        let (cell, count, mask) = self.scan();
        if count == 0 {
            return false;
        }
        if cell == usize::MAX {
            return true;
        }
        // Candidate digits (ascending == iter_digits order) on a stack array, then
        // shuffled — same `n` elements in the same order as the scalar fill, so
        // the RNG stream and produced grid are byte-identical.
        let mut digits = [0u8; 9];
        let mut n = 0;
        let mut m = mask;
        while m != 0 {
            digits[n] = m.trailing_zeros() as Digit + 1;
            m &= m - 1;
            n += 1;
        }
        rng.shuffle(&mut digits[..n]);
        for &d in &digits[..n] {
            // Back up only the bitboard (9+1 u128s = 160 B); `cells[cell]` is reset
            // explicitly. Cheaper than the old 243 B `Board` clone, and the strip's
            // ~1.7 backtracks/grid make even that rare.
            let bu_board = self.board;
            let bu_unsolved = self.unsolved;
            self.place(cell, d);
            if self.fill(rng) {
                return true;
            }
            self.board = bu_board;
            self.unsolved = bu_unsolved;
            self.cells[cell] = 0;
        }
        false
    }
}

/// A generated puzzle and the full solution it was stripped from.
pub struct GeneratedPuzzle {
    pub puzzle: Board,
    pub solution: Board,
    pub givens: usize,
}

/// Why a single attempt ended.
pub enum AttemptResult {
    /// A puzzle satisfying the spec (passed verify).
    Success(GeneratedPuzzle),
    /// A requirement-meeting `best` was found but verify rejected it (the target
    /// was substitutable). Core's `requirement_not_forced`.
    NotForced,
    /// No strip ever met the requirement counts. Core's `requirement_never_fired`.
    NeverFired,
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
/// state machine of the warp (`crate::warp::Lane` embeds one), so the gate
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
    /// or revert. The verdict source is the only thing the two drivers differ on
    /// (the sequential one computes it inline with [`BitBoard::any_alt_solves`];
    /// the warp gets it from the packed prober).
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

/// One full strip attempt for `spec`. Mirrors core's per-attempt body.
pub fn attempt(rng: &mut Rng, spec: &Spec) -> AttemptResult {
    let baseline = spec.baseline_mask();
    let forced = spec.forced_mask();
    let solution = random_full_grid(rng);
    // Strip order — always exactly the 81 cell indices, so a fixed stack array
    // (no heap alloc per attempt) rather than a `Vec`. The shuffle and iteration
    // order are unchanged, so the RNG stream and produced puzzle are identical.
    let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
    rng.shuffle(&mut positions);

    let mut st = StripState::new(&solution);
    for i in positions {
        if st.cells[i] == 0 {
            continue;
        }
        let orig = st.cells[i];
        let alts = st.strip(i, orig);
        if alts == 0 {
            st.keep_trivial(spec);
            continue;
        }
        // Uniqueness gate (inline scalar prober), then baseline gate + accept.
        let nonunique = st.bb.any_alt_solves(i, alts);
        st.resolve_gate(i, orig, nonunique, spec, baseline, forced);
    }

    match st.best {
        Some(snap) => {
            let seed = board_from_cells(&snap);
            if verify(&seed, spec) {
                let givens = seed.givens();
                AttemptResult::Success(GeneratedPuzzle { puzzle: seed, solution, givens })
            } else {
                AttemptResult::NotForced
            }
        }
        None => AttemptResult::NeverFired,
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

/// Generate until a puzzle satisfying `spec` is found or `max_attempts` is hit.
/// Returns the puzzle (if any) and the tallies up to that point.
pub fn generate(rng: &mut Rng, spec: &Spec, max_attempts: usize) -> (Option<GeneratedPuzzle>, Stats) {
    let mut stats = Stats::default();
    for _ in 0..max_attempts {
        stats.attempts += 1;
        match attempt(rng, spec) {
            AttemptResult::Success(p) => {
                stats.successes += 1;
                stats.total_givens += p.givens;
                return (Some(p), stats);
            }
            AttemptResult::NotForced => stats.not_forced += 1,
            AttemptResult::NeverFired => stats.never_fired += 1,
        }
    }
    (None, stats)
}

/// Run exactly `n` attempts (NOT "until found") for `spec` — a fixed-work,
/// deterministic benchmark of the per-attempt cost mix. Returns the tallies plus
/// a fingerprint over produced puzzles (prevents dead-code elimination and lets
/// native/wasm cross-check they did identical work).
pub fn run_attempts(rng: &mut Rng, spec: &Spec, n: usize) -> (Stats, u64) {
    let mut stats = Stats::default();
    let mut fp: u64 = FNV_OFFSET;
    for _ in 0..n {
        stats.attempts += 1;
        match attempt(rng, spec) {
            AttemptResult::Success(p) => {
                stats.successes += 1;
                stats.total_givens += p.givens;
                fnv_fold_cells(&mut fp, p.puzzle.cells());
            }
            AttemptResult::NotForced => {
                stats.not_forced += 1;
                fp = fp.wrapping_mul(FNV_PRIME);
            }
            AttemptResult::NeverFired => {
                stats.never_fired += 1;
                fp = fp.wrapping_mul(FNV_PRIME);
            }
        }
    }
    (stats, fp)
}

/// Cross-backend determinism fingerprint over `n` attempts' worth of the RNG
/// stream. Unlike [`run_attempts`]'s fp (which only folds *successful* puzzles,
/// so it is blind to the grids when nothing succeeds), this folds the full
/// solution grid AND the shuffled strip order of every iteration — the two and
/// only two RNG consumers of an attempt (the prober and baseline take no RNG).
/// Native and wasm32 MUST return the same value; that is the guard that the
/// Lemire `range`/shuffle and the bitboard fill are target-independent. It walks
/// the identical RNG trajectory as `n` attempts (the strip consumes no RNG), so
/// it is a faithful probe. This is a correctness guard, not a perf metric.
pub fn determinism_fp(rng: &mut Rng, n: usize) -> u64 {
    let mut fp: u64 = FNV_OFFSET;
    for _ in 0..n {
        let solution = random_full_grid(rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        for i in 0..CELLS {
            fp ^= solution.cell(i) as u64;
            fp = fp.wrapping_mul(FNV_PRIME);
            fp ^= positions[i] as u64;
            fp = fp.wrapping_mul(FNV_PRIME);
        }
    }
    fp
}
