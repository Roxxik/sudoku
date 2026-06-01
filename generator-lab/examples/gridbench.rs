//! Grid-build micro-profiler: the per-attempt work BEFORE the strip loop, split
//! into its sub-steps so we can see where the "grid" phase actually goes and
//! tune it data-driven. Times each step over `iters` independent grids
//! (re-seeded identically per step so each step does the *same* RNG work) and
//! reports fill backtracking stats (~83 nodes/grid, ~1.7 backtracks — so the
//! per-node backup is almost never restored and the MRV *scan* is the cost).
//!
//! It is also the experiment record for choosing the fill representation. All
//! variants below produce byte-identical grids (the matching `fp` proves it);
//! `random_full_grid` (the "fill" line) is now the **bitboard `u128` + sieve
//! MRV** winner, sieve capped at tier 3 (exp G2, ~2.13x over the old scalar
//! maintained-candidate fill).
//!
//! The losers are kept as evidence:
//!  - B (masks, no clone) and C (undo log, no clone) both *regress* — they tax
//!    the hot scan, the actual bottleneck; the clone is not it.
//!  - E (maintained count[]) wins but the bitboard rep (G) beats it.
//!  - G = full 9-tier sieve; G2 = sieve capped at tier 3 (~9% over G, shipped);
//!    G3 = G2 + last-digit no-backup was a wash (re-confirming the backup copy
//!    isn't a meaningful cost).
//!  - The SIMD-dense `(score<<7)|idx` reduce-min scan is a native win but the
//!    worst option on ARM (no 16-bit simd128 popcount), so it is absent.
//!
//! Usage: cargo run --release -p generator-lab --example gridbench -- [--iters N=200000] [--seed S=1]

use std::time::Instant;

use generator_lab::bb::{BitBoard, Placed};
use generator_lab::generator::random_full_grid;
use generator_lab::grid::{Board, CELLS, Digit};
use generator_lab::rng::Rng;

fn main() {
    let mut iters = 200_000usize;
    let mut seed = 1u64;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            "--seed" => seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(seed),
            _ => {}
        }
    }

    println!("grid-build micro-profile: {iters} grids, seed {seed}\n");

    // Step 1: random_full_grid (MRV fill). Fingerprint the grids to keep the
    // optimizer honest and confirm later rewrites produce identical grids.
    let mut rng = Rng::from_seed(seed);
    let mut fp: u64 = 0xcbf29ce484222325;
    let t = Instant::now();
    let mut last = Board::empty();
    for _ in 0..iters {
        let g = random_full_grid(&mut rng);
        fp ^= g.cell(0) as u64 ^ ((g.cell(40) as u64) << 8) ^ ((g.cell(80) as u64) << 16);
        fp = fp.wrapping_mul(0x100000001b3);
        last = g;
    }
    let fill_ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;

    // Step 2: positions setup + shuffle (a fixed [usize; 81] stack array, as the
    // shipped `attempt` now does — no per-attempt heap alloc).
    let mut rng = Rng::from_seed(seed);
    let t = Instant::now();
    let mut sink = 0usize;
    for _ in 0..iters {
        let _g = random_full_grid(&mut rng); // keep stream identical to attempt()
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        sink ^= positions[0];
    }
    let fill_plus_pos_ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;

    // Step 3: bb + placed + cells construction (on the same grid).
    let mut rng = Rng::from_seed(seed);
    let t = Instant::now();
    let mut bbsink = 0u32;
    for _ in 0..iters {
        let g = random_full_grid(&mut rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        let bb = BitBoard::from_board(&g);
        let _placed = Placed::from_board(&g);
        let _cells: [Digit; CELLS] = core::array::from_fn(|i| g.cell(i));
        bbsink ^= bb.count_empties();
    }
    let full_ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;

    let pos_ns = fill_plus_pos_ns - fill_ns;
    let bb_ns = full_ns - fill_plus_pos_ns;

    println!("  fill (random_full_grid) {:>8.1} ns/grid", fill_ns);
    println!("  positions array+shuffle {:>8.1} ns/grid", pos_ns);
    println!("  bb+placed+cells build   {:>8.1} ns/grid", bb_ns);
    println!("  --------------------------------------");
    println!("  grid-build total        {:>8.1} ns/grid", full_ns);
    println!();
    println!("  (fp {:#018x}, sink {}, bbsink {}, last givens {})", fp, sink, bbsink, last.givens());
    println!();
    count_nodes(iters, seed);
    println!();
    bench_maskfill(iters, seed);
    bench_logfill(iters, seed);
    bench_countfill(iters, seed);
    bench_bbfill(iters, seed);
    bench_bbfill2(iters, seed);
    bench_bbfill3(iters, seed);
    hist_mintier(iters, seed);
}

// --- throwaway: instrumented fill to count search nodes vs the 81 placements ---
// (mirrors generator::fill exactly, plus node/backtrack counters)
use generator_lab::grid::{popcount, Digit as D2};

fn fill_counted(board: &mut Board, rng: &mut Rng, nodes: &mut u64, backtracks: &mut u64) -> bool {
    let mut best: Option<(usize, u16, u32)> = None;
    for i in 0..CELLS {
        if !board.is_empty(i) { continue; }
        let cs = board.candidates(i);
        let n = popcount(cs);
        if n == 0 { return false; }
        if best.map_or(true, |(_, _, bn)| n < bn) { best = Some((i, cs, n)); }
    }
    let Some((cell, mask, _)) = best else { return true; };
    let mut digits = [0u8; 9];
    let mut n = 0; let mut m = mask;
    while m != 0 { digits[n] = m.trailing_zeros() as D2 + 1; m &= m - 1; n += 1; }
    rng.shuffle(&mut digits[..n]);
    for &d in &digits[..n] {
        *nodes += 1;
        let backup = board.clone();
        board.place(cell, d);
        if fill_counted(board, rng, nodes, backtracks) { return true; }
        *board = backup;
        *backtracks += 1;
    }
    false
}

#[allow(dead_code)]
fn count_nodes(iters: usize, seed: u64) {
    let mut rng = Rng::from_seed(seed);
    let mut nodes = 0u64; let mut backtracks = 0u64;
    for _ in 0..iters {
        let mut b = Board::empty();
        fill_counted(&mut b, &mut rng, &mut nodes, &mut backtracks);
    }
    println!("  fill nodes/grid {:.2}  (81 are real placements; backtracks/grid {:.3})",
        nodes as f64 / iters as f64, backtracks as f64 / iters as f64);
}

// --- prototype B: allocation-free fill via row/col/box used-digit masks ------
// No per-cell candidate array, no clone: place sets 3 bits, undo clears 3 bits.
// candidates(i) = ALL & !(used_r[row] | used_c[col] | used_b[box]). Visits cells
// 0..81, first strict-min MRV, digits ascending then shuffled -> byte-identical
// grid + RNG stream to generator::fill (proven by the matching fp below).
use generator_lab::grid::{ALL_DIGITS, box_of, col_of, row_of};

struct MaskFill {
    cells: [u8; 81],
    ur: [u16; 9],
    uc: [u16; 9],
    ub: [u16; 9],
}

impl MaskFill {
    fn cand(&self, i: usize) -> u16 {
        ALL_DIGITS & !(self.ur[row_of(i)] | self.uc[col_of(i)] | self.ub[box_of(i)])
    }
    fn fill(&mut self, rng: &mut Rng) -> bool {
        let mut best: Option<(usize, u16, u32)> = None;
        for i in 0..CELLS {
            if self.cells[i] != 0 { continue; }
            let cs = self.cand(i);
            let n = cs.count_ones();
            if n == 0 { return false; }
            if best.map_or(true, |(_, _, bn)| n < bn) { best = Some((i, cs, n)); }
        }
        let Some((cell, mask, _)) = best else { return true; };
        let mut digits = [0u8; 9];
        let mut n = 0; let mut m = mask;
        while m != 0 { digits[n] = m.trailing_zeros() as u8 + 1; m &= m - 1; n += 1; }
        rng.shuffle(&mut digits[..n]);
        let (r, c, b) = (row_of(cell), col_of(cell), box_of(cell));
        for &d in &digits[..n] {
            let bit = 1u16 << (d - 1);
            self.cells[cell] = d;
            self.ur[r] |= bit; self.uc[c] |= bit; self.ub[b] |= bit;
            if self.fill(rng) { return true; }
            self.cells[cell] = 0;
            self.ur[r] &= !bit; self.uc[c] &= !bit; self.ub[b] &= !bit;
        }
        false
    }
}

#[allow(dead_code)]
fn bench_maskfill(iters: usize, seed: u64) {
    let mut rng = Rng::from_seed(seed);
    let mut fp: u64 = 0xcbf29ce484222325;
    let t = Instant::now();
    for _ in 0..iters {
        let mut f = MaskFill { cells: [0; 81], ur: [0; 9], uc: [0; 9], ub: [0; 9] };
        f.fill(&mut rng);
        fp ^= f.cells[0] as u64 ^ ((f.cells[40] as u64) << 8) ^ ((f.cells[80] as u64) << 16);
        fp = fp.wrapping_mul(0x100000001b3);
    }
    let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
    println!("  maskfill (prototype B)  {:>8.1} ns/grid   (fp {:#018x})", ns, fp);
}

// --- prototype C: keep the maintained candidate array (cheap 1-load MRV scan)
// but replace the per-node Board::clone with an incremental undo log. place
// records which peers it actually cleared bit d from; backtrack re-ORs exactly
// those. Isolates "is the 20 KB/grid clone memcpy worth removing, holding the
// fast scan?". Identical grids+stream (fp must match).
use generator_lab::grid::PEERS;

struct LogFill {
    cells: [u8; 81],
    cand: [u16; 81],
}

impl LogFill {
    fn fill(&mut self, rng: &mut Rng) -> bool {
        let mut best: Option<(usize, u16, u32)> = None;
        for i in 0..CELLS {
            if self.cells[i] != 0 { continue; }
            let cs = self.cand[i];
            let n = cs.count_ones();
            if n == 0 { return false; }
            if best.map_or(true, |(_, _, bn)| n < bn) { best = Some((i, cs, n)); }
        }
        let Some((cell, mask, _)) = best else { return true; };
        let mut digits = [0u8; 9];
        let mut n = 0; let mut m = mask;
        while m != 0 { digits[n] = m.trailing_zeros() as u8 + 1; m &= m - 1; n += 1; }
        rng.shuffle(&mut digits[..n]);
        for &d in &digits[..n] {
            let bit = 1u16 << (d - 1);
            // place with logging
            self.cells[cell] = d;
            self.cand[cell] = 0;
            let mut changed = [0u8; 20];
            let mut nch = 0usize;
            for &p in &PEERS[cell] {
                if self.cand[p] & bit != 0 {
                    self.cand[p] &= !bit;
                    changed[nch] = p as u8; nch += 1;
                }
            }
            if self.fill(rng) { return true; }
            // undo from log
            self.cells[cell] = 0;
            self.cand[cell] = mask;
            for &p in &changed[..nch] { self.cand[p as usize] |= bit; }
        }
        false
    }
}

#[allow(dead_code)]
fn bench_logfill(iters: usize, seed: u64) {
    let mut rng = Rng::from_seed(seed);
    let mut fp: u64 = 0xcbf29ce484222325;
    let t = Instant::now();
    for _ in 0..iters {
        let mut f = LogFill { cells: [0; 81], cand: [ALL_DIGITS; 81] };
        f.fill(&mut rng);
        fp ^= f.cells[0] as u64 ^ ((f.cells[40] as u64) << 8) ^ ((f.cells[80] as u64) << 16);
        fp = fp.wrapping_mul(0x100000001b3);
    }
    let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
    println!("  logfill (prototype C)   {:>8.1} ns/grid   (fp {:#018x})", ns, fp);
}

// === Experiment E: maintained per-cell candidate count =======================
// The scalar scan's per-visit cost is a candidates[i] load + popcount. Keep a
// count[i] (u8) maintained on place (decrement a peer only when its bit was
// actually set) so the MRV scan reads a u8 and compares -- no popcount on the
// hot path. ARM-neutral (pure integer ops), so native is a fair filter here.
// The clone grows by the 81-byte count array. Identical grids+stream.
#[derive(Clone)]
struct CountFill {
    cells: [u8; 81],
    cand: [u16; 81],
    cnt: [u8; 81],
}

impl CountFill {
    fn fill(&mut self, rng: &mut Rng) -> bool {
        let mut bn = u32::MAX;
        let mut bc = usize::MAX;
        for i in 0..CELLS {
            if self.cells[i] != 0 { continue; }
            let n = self.cnt[i] as u32;
            if n == 0 { return false; }
            if n < bn { bn = n; bc = i; }
        }
        if bc == usize::MAX { return true; }
        let cell = bc;
        let mut digits = [0u8; 9];
        let mut n = 0; let mut m = self.cand[cell];
        while m != 0 { digits[n] = m.trailing_zeros() as u8 + 1; m &= m - 1; n += 1; }
        rng.shuffle(&mut digits[..n]);
        for &d in &digits[..n] {
            let backup = self.clone();
            let bit = 1u16 << (d - 1);
            self.cells[cell] = d;
            self.cand[cell] = 0;
            for &p in &PEERS[cell] {
                if self.cand[p] & bit != 0 { self.cand[p] &= !bit; self.cnt[p] -= 1; }
            }
            if self.fill(rng) { return true; }
            *self = backup;
        }
        false
    }
}

#[allow(dead_code)]
fn bench_countfill(iters: usize, seed: u64) {
    let mut rng = Rng::from_seed(seed);
    let mut fp: u64 = 0xcbf29ce484222325;
    let t = Instant::now();
    for _ in 0..iters {
        let mut f = CountFill { cells: [0; 81], cand: [ALL_DIGITS; 81], cnt: [9; 81] };
        f.fill(&mut rng);
        fp ^= f.cells[0] as u64 ^ ((f.cells[40] as u64) << 8) ^ ((f.cells[80] as u64) << 16);
        fp = fp.wrapping_mul(0x100000001b3);
    }
    let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
    println!("  countfill (exp E)       {:>8.1} ns/grid   (fp {:#018x})", ns, fp);
}

// === Experiment G: digit-transposed u128 bitboard fill =======================
// solver-lab's ARM-winning `bitboard` rep applied to the FILL: nine 81-bit
// boards (board[d] bit c set iff digit d+1 still fits cell c) + an `unsolved`
// mask. place = two u128 AND-NOTs (no 20-peer walk). MRV via the popcount-free
// symmetric sieve: a[k] = unsolved cells with >= k candidates; the lowest
// non-empty exactly-k tier, lowest bit = lowest-index min-count cell == the
// scalar fill's pick. Clone is 9*16+16 = 160 B (< Board's 243). Identical grids.
const fn build_peer_mask_u128() -> [u128; CELLS] {
    let mut m = [0u128; CELLS];
    let mut i = 0;
    while i < CELLS {
        let mut k = 0;
        while k < 20 { m[i] |= 1u128 << PEERS[i][k]; k += 1; }
        i += 1;
    }
    m
}
const PEER_MASK_U128: [u128; CELLS] = build_peer_mask_u128();

#[derive(Clone)]
struct BbFill {
    board: [u128; 9],
    unsolved: u128,
    cells: [u8; 81],
}

impl BbFill {
    #[inline]
    fn scan(&self) -> (usize, u32, u16) {
        let mut a = [0u128; 11]; // a[1..=9] used; a[10] stays 0 as the k==9 upper
        for d in 0..9 {
            let b = self.board[d] & self.unsolved;
            let mut k = 9;
            while k >= 2 { a[k] |= a[k - 1] & b; k -= 1; }
            a[1] |= b;
        }
        if self.unsolved & !a[1] != 0 { return (usize::MAX, 0, 0); } // dead cell
        if self.unsolved == 0 { return (usize::MAX, 10, 0); }        // solved
        for k in 1..=9usize {
            let tier = a[k] & !a[k + 1];
            if tier != 0 {
                let cell = tier.trailing_zeros() as usize;
                let cb = 1u128 << cell;
                let mut mask = 0u16;
                for dd in 0..9 { if self.board[dd] & cb != 0 { mask |= 1 << dd; } }
                return (cell, k as u32, mask);
            }
        }
        unreachable!()
    }
    #[inline]
    fn place(&mut self, cell: usize, d: u8) {
        self.unsolved &= !(1u128 << cell);
        self.board[(d - 1) as usize] &= !PEER_MASK_U128[cell];
        self.cells[cell] = d;
    }
    fn fill(&mut self, rng: &mut Rng) -> bool {
        let (cell, count, mask) = self.scan();
        if count == 0 { return false; }
        if cell == usize::MAX { return true; }
        let mut digits = [0u8; 9];
        let mut n = 0; let mut m = mask;
        while m != 0 { digits[n] = m.trailing_zeros() as u8 + 1; m &= m - 1; n += 1; }
        rng.shuffle(&mut digits[..n]);
        for &d in &digits[..n] {
            let bu_board = self.board;
            let bu_un = self.unsolved;
            self.place(cell, d);
            if self.fill(rng) { return true; }
            self.board = bu_board;
            self.unsolved = bu_un;
            self.cells[cell] = 0;
        }
        false
    }
}

#[allow(dead_code)]
fn bench_bbfill(iters: usize, seed: u64) {
    let all_cells: u128 = (1u128 << CELLS) - 1;
    let mut rng = Rng::from_seed(seed);
    let mut fp: u64 = 0xcbf29ce484222325;
    let t = Instant::now();
    for _ in 0..iters {
        let mut f = BbFill { board: [all_cells; 9], unsolved: all_cells, cells: [0; 81] };
        f.fill(&mut rng);
        fp ^= f.cells[0] as u64 ^ ((f.cells[40] as u64) << 8) ^ ((f.cells[80] as u64) << 16);
        fp = fp.wrapping_mul(0x100000001b3);
    }
    let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
    println!("  bbfill (exp G)          {:>8.1} ns/grid   (fp {:#018x})", ns, fp);
}

// --- min-tier distribution over fill nodes (justifies a tier-capped sieve) ----
fn bb_scan_mintier(board: &[u128; 9], unsolved: u128) -> usize {
    // returns the minimum candidate count over unsolved cells (1..9), or 0 if solved
    if unsolved == 0 { return 0; }
    let mut a = [0u128; 11];
    for d in 0..9 {
        let b = board[d] & unsolved;
        let mut k = 9; while k >= 2 { a[k] |= a[k - 1] & b; k -= 1; } a[1] |= b;
    }
    for k in 1..=9usize { if a[k] & !a[k + 1] != 0 { return k; } }
    0
}

#[allow(dead_code)]
fn hist_mintier(iters: usize, seed: u64) {
    let all_cells: u128 = (1u128 << CELLS) - 1;
    let mut rng = Rng::from_seed(seed);
    let mut hist = [0u64; 10];
    // reimplement the fill but tally min tier at each node
    fn rec(board: &mut [u128; 9], unsolved: &mut u128, rng: &mut Rng, hist: &mut [u64; 10]) -> bool {
        let mt = bb_scan_mintier(board, *unsolved);
        hist[mt] += 1;
        if mt == 0 { return true; } // solved (no dead-ends on a valid fill path until backtrack)
        // find the chosen cell + mask the same way as scan
        let mut a = [0u128; 11];
        for d in 0..9 { let b = board[d] & *unsolved; let mut k=9; while k>=2 {a[k]|=a[k-1]&b;k-=1;} a[1]|=b; }
        if *unsolved & !a[1] != 0 { return false; }
        let tier = a[mt] & !a[mt + 1];
        let cell = tier.trailing_zeros() as usize;
        let cb = 1u128 << cell;
        let mut mask = 0u16;
        for d in 0..9 { if board[d] & cb != 0 { mask |= 1 << d; } }
        let mut digits = [0u8; 9]; let mut n=0; let mut m=mask;
        while m != 0 { digits[n]=m.trailing_zeros() as u8+1; m&=m-1; n+=1; }
        rng.shuffle(&mut digits[..n]);
        for &dd in &digits[..n] {
            let bb=*board; let bu=*unsolved;
            *unsolved &= !cb; board[(dd-1)as usize] &= !PEER_MASK_U128[cell];
            if rec(board, unsolved, rng, hist) { return true; }
            *board=bb; *unsolved=bu;
        }
        false
    }
    for _ in 0..iters {
        let mut board = [all_cells; 9];
        let mut unsolved = all_cells;
        rec(&mut board, &mut unsolved, &mut rng, &mut hist);
    }
    let total: u64 = hist.iter().sum();
    print!("  min-tier histogram (per scan, {} scans/grid):", total / iters as u64);
    for k in 1..=9 { print!(" {}:{:.1}%", k, 100.0 * hist[k] as f64 / total as f64); }
    println!(" solved:{:.1}%", 100.0 * hist[0] as f64 / total as f64);
}

// === Experiment G2: tier-capped sieve ========================================
// MRV only needs the LOWEST non-empty tier, and 82.8% of scans have min tier <=3
// (measured: 1:41.8% 2:27.8% 3:13.2%). So compute only the first four levels
// (ones/twos/threes/fours -> 7 u128 ops/digit vs the full sieve's 17) and pick
// from tiers 1-3; only the ~16% of nodes whose every unsolved cell has >=4
// candidates (early board) fall back to the full sieve. Identical grids+stream.
#[derive(Clone)]
struct BbFill2 {
    board: [u128; 9],
    unsolved: u128,
    cells: [u8; 81],
}

impl BbFill2 {
    #[inline]
    fn pick(&self, tier: u128, k: u32) -> (usize, u32, u16) {
        let cell = tier.trailing_zeros() as usize;
        let cb = 1u128 << cell;
        let mut mask = 0u16;
        for d in 0..9 { if self.board[d] & cb != 0 { mask |= 1 << d; } }
        (cell, k, mask)
    }
    #[inline]
    fn scan(&self) -> (usize, u32, u16) {
        let u = self.unsolved;
        // Capped sieve: ones..fours = unsolved cells with >=1..>=4 candidates.
        let (mut ones, mut twos, mut threes, mut fours) = (0u128, 0u128, 0u128, 0u128);
        for d in 0..9 {
            let b = self.board[d] & u;
            fours |= threes & b;
            threes |= twos & b;
            twos |= ones & b;
            ones |= b;
        }
        if u & !ones != 0 { return (usize::MAX, 0, 0); } // dead unsolved cell
        if u == 0 { return (usize::MAX, 10, 0); }         // solved
        let t1 = ones & !twos;
        if t1 != 0 { return self.pick(t1, 1); }
        let t2 = twos & !threes;
        if t2 != 0 { return self.pick(t2, 2); }
        let t3 = threes & !fours;
        if t3 != 0 { return self.pick(t3, 3); }
        // Rare (~16%): every unsolved cell has >=4 candidates. Full sieve for 4..9.
        let mut a = [0u128; 11];
        for d in 0..9 {
            let b = self.board[d] & u;
            let mut k = 9; while k >= 2 { a[k] |= a[k - 1] & b; k -= 1; } a[1] |= b;
        }
        for k in 4..=9usize {
            let tier = a[k] & !a[k + 1];
            if tier != 0 { return self.pick(tier, k as u32); }
        }
        unreachable!()
    }
    #[inline]
    fn place(&mut self, cell: usize, d: u8) {
        self.unsolved &= !(1u128 << cell);
        self.board[(d - 1) as usize] &= !PEER_MASK_U128[cell];
        self.cells[cell] = d;
    }
    fn fill(&mut self, rng: &mut Rng) -> bool {
        let (cell, count, mask) = self.scan();
        if count == 0 { return false; }
        if cell == usize::MAX { return true; }
        let mut digits = [0u8; 9];
        let mut n = 0; let mut m = mask;
        while m != 0 { digits[n] = m.trailing_zeros() as u8 + 1; m &= m - 1; n += 1; }
        rng.shuffle(&mut digits[..n]);
        for &d in &digits[..n] {
            let bu_board = self.board; let bu_un = self.unsolved;
            self.place(cell, d);
            if self.fill(rng) { return true; }
            self.board = bu_board; self.unsolved = bu_un; self.cells[cell] = 0;
        }
        false
    }
}

#[allow(dead_code)]
fn bench_bbfill2(iters: usize, seed: u64) {
    let all_cells: u128 = (1u128 << CELLS) - 1;
    let mut rng = Rng::from_seed(seed);
    let mut fp: u64 = 0xcbf29ce484222325;
    let t = Instant::now();
    for _ in 0..iters {
        let mut f = BbFill2 { board: [all_cells; 9], unsolved: all_cells, cells: [0; 81] };
        f.fill(&mut rng);
        fp ^= f.cells[0] as u64 ^ ((f.cells[40] as u64) << 8) ^ ((f.cells[80] as u64) << 16);
        fp = fp.wrapping_mul(0x100000001b3);
    }
    let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
    println!("  bbfill2 capped (exp G2) {:>8.1} ns/grid   (fp {:#018x})", ns, fp);
}

// === Experiment G3: capped sieve + last-digit no-backup =======================
// Stacks on G2: for the LAST candidate digit at a node, place in-self and recurse
// without backing up -- if it fails, the nearest non-last-digit ancestor's restore
// undoes everything below it. Tier-1 nodes (41.8%, n==1) thus take ZERO backups.
// cells[] stale-writes from abandoned branches are harmless: the success path
// places all 81 cells, overwriting them (fp proves byte-identical output).
#[derive(Clone)]
struct BbFill3 {
    board: [u128; 9],
    unsolved: u128,
    cells: [u8; 81],
}

impl BbFill3 {
    #[inline]
    fn pick(&self, tier: u128, k: u32) -> (usize, u32, u16) {
        let cell = tier.trailing_zeros() as usize;
        let cb = 1u128 << cell;
        let mut mask = 0u16;
        for d in 0..9 { if self.board[d] & cb != 0 { mask |= 1 << d; } }
        (cell, k, mask)
    }
    #[inline]
    fn scan(&self) -> (usize, u32, u16) {
        let u = self.unsolved;
        let (mut ones, mut twos, mut threes, mut fours) = (0u128, 0u128, 0u128, 0u128);
        for d in 0..9 {
            let b = self.board[d] & u;
            fours |= threes & b; threes |= twos & b; twos |= ones & b; ones |= b;
        }
        if u & !ones != 0 { return (usize::MAX, 0, 0); }
        if u == 0 { return (usize::MAX, 10, 0); }
        let t1 = ones & !twos; if t1 != 0 { return self.pick(t1, 1); }
        let t2 = twos & !threes; if t2 != 0 { return self.pick(t2, 2); }
        let t3 = threes & !fours; if t3 != 0 { return self.pick(t3, 3); }
        let mut a = [0u128; 11];
        for d in 0..9 {
            let b = self.board[d] & u;
            let mut k = 9; while k >= 2 { a[k] |= a[k - 1] & b; k -= 1; } a[1] |= b;
        }
        for k in 4..=9usize {
            let tier = a[k] & !a[k + 1];
            if tier != 0 { return self.pick(tier, k as u32); }
        }
        unreachable!()
    }
    #[inline]
    fn place(&mut self, cell: usize, d: u8) {
        self.unsolved &= !(1u128 << cell);
        self.board[(d - 1) as usize] &= !PEER_MASK_U128[cell];
        self.cells[cell] = d;
    }
    fn fill(&mut self, rng: &mut Rng) -> bool {
        let (cell, count, mask) = self.scan();
        if count == 0 { return false; }
        if cell == usize::MAX { return true; }
        let mut digits = [0u8; 9];
        let mut n = 0; let mut m = mask;
        while m != 0 { digits[n] = m.trailing_zeros() as u8 + 1; m &= m - 1; n += 1; }
        rng.shuffle(&mut digits[..n]);
        for &d in &digits[..n - 1] {
            let bu_board = self.board; let bu_un = self.unsolved;
            self.place(cell, d);
            if self.fill(rng) { return true; }
            self.board = bu_board; self.unsolved = bu_un; self.cells[cell] = 0;
        }
        // Last digit: no backup; parent restores on failure.
        self.place(cell, digits[n - 1]);
        self.fill(rng)
    }
}

#[allow(dead_code)]
fn bench_bbfill3(iters: usize, seed: u64) {
    let all_cells: u128 = (1u128 << CELLS) - 1;
    let mut rng = Rng::from_seed(seed);
    let mut fp: u64 = 0xcbf29ce484222325;
    let t = Instant::now();
    for _ in 0..iters {
        let mut f = BbFill3 { board: [all_cells; 9], unsolved: all_cells, cells: [0; 81] };
        f.fill(&mut rng);
        fp ^= f.cells[0] as u64 ^ ((f.cells[40] as u64) << 8) ^ ((f.cells[80] as u64) << 16);
        fp = fp.wrapping_mul(0x100000001b3);
    }
    let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
    println!("  bbfill3 last-nobkp (G3) {:>8.1} ns/grid   (fp {:#018x})", ns, fp);
}
