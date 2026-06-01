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
pub enum Outcome {
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

/// One full strip attempt for `spec`. Mirrors core's per-attempt body.
pub fn attempt(rng: &mut Rng, spec: &Spec) -> Outcome {
    let baseline = spec.baseline_mask();
    let forced = spec.forced_mask();
    let solution = random_full_grid(rng);
    // Strip order — always exactly the 81 cell indices, so a fixed stack array
    // (no heap alloc per attempt) rather than a `Vec`. The shuffle and iteration
    // order are unchanged, so the RNG stream and produced puzzle are identical.
    let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
    rng.shuffle(&mut positions);

    // `bb` is the single source of candidate truth (it already holds every
    // candidate band). The only scalar shadow kept is the bare puzzle grid
    // `cells` (present cell = its digit, `0` = stripped) — the one thing bb can't
    // derive: which digit a surviving peer holds. `bb` is maintained
    // incrementally — a clear/place only touches cell i and its peers — and
    // `apply_clear` reads the reopened candidates straight off `cells`, so the
    // duplicate per-cell *candidate* array (and its upkeep) is gone, as is any
    // per-position `from_board` rebuild.
    let mut bb = BitBoard::from_board(&solution);
    let mut placed = Placed::from_board(&solution);
    let mut cells: [Digit; CELLS] = core::array::from_fn(|i| solution.cell(i));

    // `best` is the bare-grid snapshot of the most-stripped requirement-meeting
    // state; the candidate board is rebuilt from it only if the attempt succeeds.
    // `req_met` carries the running requirement verdict of the current accepted
    // board (see the `alts == 0` fast path below). The full grid is trivially
    // baseline-solvable but fires nothing, so it starts false.
    let mut best: Option<[Digit; CELLS]> = None;
    let mut req_met = false;
    for i in positions {
        if cells[i] == 0 {
            continue;
        }
        let orig = cells[i];
        cells[i] = 0;
        let cand = bb.apply_clear(i, orig, &mut placed);
        debug_assert!(
            bb == BitBoard::from_board(&board_from_cells(&cells)),
            "bb desync after clear at {i}"
        );

        let v_bit = digit_to_bit(orig);
        let alts = cand & !v_bit;

        // Fast path: clearing `i` left it with only its own digit, i.e. `i` is
        // still a naked single. The strip is therefore always valid — the
        // baseline would re-place `i` immediately and reach a byte-identical
        // closure — so it stays unique AND baseline-solvable, and the requirement
        // verdict is unchanged. Both gates are skippable; just carry `req_met`.
        if alts == 0 {
            debug_assert!(
                {
                    let o = bb.baseline(baseline, forced);
                    o.solved && spec.requirement_met(&o.counts) == req_met
                },
                "alts==0 fast-path invariant broke at {i}"
            );
            if req_met {
                best = Some(cells);
            }
            continue;
        }

        // Uniqueness gate.
        if bb.any_alt_solves(i, alts) {
            cells[i] = orig;
            bb.apply_place(i, orig, &mut placed);
            debug_assert!(
                bb == BitBoard::from_board(&board_from_cells(&cells)),
                "bb desync after revert at {i}"
            );
            continue;
        }

        // Baseline gate + requirement counts (one tracked solve).
        let outcome = bb.baseline(baseline, forced);
        if !outcome.solved {
            cells[i] = orig;
            bb.apply_place(i, orig, &mut placed);
            debug_assert!(
                bb == BitBoard::from_board(&board_from_cells(&cells)),
                "bb desync after revert at {i}"
            );
            continue;
        }
        // Accept the strip: `cells` and `bb` stay in the stripped state.
        req_met = spec.requirement_met(&outcome.counts);
        if req_met {
            best = Some(cells);
        }
    }

    match best {
        Some(snap) => {
            let seed = board_from_cells(&snap);
            if verify(&seed, spec) {
                let givens = seed.givens();
                Outcome::Success(GeneratedPuzzle {
                    puzzle: seed,
                    solution,
                    givens,
                })
            } else {
                Outcome::NotForced
            }
        }
        None => Outcome::NeverFired,
    }
}

/// Per-run tallies, for throughput/yield reporting.
#[derive(Default, Clone, Copy)]
pub struct GenStats {
    pub attempts: usize,
    pub successes: usize,
    pub never_fired: usize,
    pub not_forced: usize,
    pub total_givens: usize,
}

/// Generate until a puzzle satisfying `spec` is found or `max_attempts` is hit.
/// Returns the puzzle (if any) and the tallies up to that point.
pub fn generate(rng: &mut Rng, spec: &Spec, max_attempts: usize) -> (Option<GeneratedPuzzle>, GenStats) {
    let mut stats = GenStats::default();
    for _ in 0..max_attempts {
        stats.attempts += 1;
        match attempt(rng, spec) {
            Outcome::Success(p) => {
                stats.successes += 1;
                stats.total_givens += p.givens;
                return (Some(p), stats);
            }
            Outcome::NotForced => stats.not_forced += 1,
            Outcome::NeverFired => stats.never_fired += 1,
        }
    }
    (None, stats)
}

/// Run exactly `n` attempts (NOT "until found") for `spec` — a fixed-work,
/// deterministic benchmark of the per-attempt cost mix. Returns the tallies plus
/// a fingerprint over produced puzzles (prevents dead-code elimination and lets
/// native/wasm cross-check they did identical work).
pub fn run_attempts(rng: &mut Rng, spec: &Spec, n: usize) -> (GenStats, u64) {
    let mut stats = GenStats::default();
    let mut fp: u64 = 0xcbf29ce484222325;
    for _ in 0..n {
        stats.attempts += 1;
        match attempt(rng, spec) {
            Outcome::Success(p) => {
                stats.successes += 1;
                stats.total_givens += p.givens;
                for i in 0..CELLS {
                    fp ^= p.puzzle.cell(i) as u64;
                    fp = fp.wrapping_mul(0x100000001b3);
                }
            }
            Outcome::NotForced => {
                stats.not_forced += 1;
                fp = fp.wrapping_mul(0x100000001b3);
            }
            Outcome::NeverFired => {
                stats.never_fired += 1;
                fp = fp.wrapping_mul(0x100000001b3);
            }
        }
    }
    (stats, fp)
}
