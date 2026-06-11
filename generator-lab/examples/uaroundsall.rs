//! Exhaustive worst-case round count for the packed UA box-merge min-relaxation.
//!
//! `examples/uarounds` samples random full boards; its 36M-board max (4) is a lower bound
//! on the true worst case, while the generic graph argument in `enumerate_2digit_packed`
//! proves 7. This rig closes that gap exactly: the relaxation structure (`pi`, `pii`,
//! `na`, `nb`) for a digit pair depends only on where those two digits sit, and in any
//! completed grid two digits form a pair of disjoint single-digit placements (one cell
//! per row, column and box; 46656 such placements). Enumerating ALL unordered disjoint
//! placement pairs therefore covers every 2-digit configuration any board can contain --
//! a sampling-free upper bound. For every rounds level above the sampled max the rig then
//! tries to complete stored example pairs to a full grid, deciding whether the level is
//! realizable in a real board at all or only by non-completable placement pairs.
//!
//! Usage: cargo run --release -p generator-lab --example uaroundsall [threads]

use std::thread;

const N_PLACEMENTS: usize = 46656;
/// Per-thread cap on stored example pairs per rounds level (>= REALIZE_FROM); the
/// summary states how many pairs exist vs. how many were stored, so the cap is visible.
const EXAMPLE_CAP: usize = 256;
/// Levels at or above this get a completability check (4 is already realized by the
/// 36M-board sample in `uarounds`).
const REALIZE_FROM: usize = 5;

#[derive(Clone, Copy)]
struct Placement {
    cols: [u8; 9],   // row -> col
    inv: [u8; 9],    // col -> row
    boxrow: [u8; 9], // box -> row
    packed: u64,     // cols as 9 nibbles, for the zero-nibble disjointness test
}

fn placements() -> Vec<Placement> {
    fn rec(row: usize, used: u16, cols: &mut [u8; 9], out: &mut Vec<Placement>) {
        if row == 9 {
            let mut inv = [0u8; 9];
            let mut boxrow = [0u8; 9];
            let mut packed = 0u64;
            for r in 0..9 {
                let c = cols[r] as usize;
                inv[c] = r as u8;
                boxrow[(r / 3) * 3 + c / 3] = r as u8;
                packed |= (c as u64) << (4 * r);
            }
            out.push(Placement { cols: *cols, inv, boxrow, packed });
            return;
        }
        // Box property: within a band the three rows use three distinct stacks.
        let mut band_stacks = 0u8;
        for r in (row / 3) * 3..row {
            band_stacks |= 1 << (cols[r] / 3);
        }
        for c in 0..9u8 {
            if used & (1 << c) != 0 || band_stacks & (1 << (c / 3)) != 0 {
                continue;
            }
            cols[row] = c;
            rec(row + 1, used | (1 << c), cols, out);
        }
    }
    let mut out = Vec::with_capacity(N_PLACEMENTS);
    rec(0, 0, &mut [0u8; 9], &mut out);
    assert_eq!(out.len(), N_PLACEMENTS);
    out
}

/// Two placements share no cell. Shared cell <=> some nibble of `a ^ b` is zero
/// (same column in some row); the borrow trick is boolean-exact for zero-nibble detection.
fn disjoint(a: u64, b: u64) -> bool {
    const ONES: u64 = 0x1_1111_1111;
    let x = a ^ b;
    (x.wrapping_sub(ONES) & !x & (ONES << 3)) == 0
}

/// Rounds of min-relaxation to reach a fixpoint from `lab` over the four neighbor maps
/// (same semantics as `examples/uarounds`: production needs ROUNDS >= this).
fn rounds_to_fixpoint(
    lab: &[u8; 9],
    pi: &[u8; 9],
    pii: &[u8; 9],
    na: &[u8; 9],
    nb: &[u8; 9],
) -> usize {
    let mut cur = *lab;
    let mut rounds = 0;
    loop {
        let mut next = cur;
        for r in 0..9 {
            next[r] = cur[r]
                .min(cur[pi[r] as usize])
                .min(cur[pii[r] as usize])
                .min(cur[na[r] as usize])
                .min(cur[nb[r] as usize]);
        }
        if next == cur {
            return rounds;
        }
        cur = next;
        rounds += 1;
    }
}

fn pair_rounds(a: &Placement, b: &Placement) -> usize {
    let mut pi = [0u8; 9];
    let mut pii = [0u8; 9];
    let mut na = [0u8; 9];
    let mut nb = [0u8; 9];
    for r in 0..9 {
        pi[r] = b.inv[a.cols[r] as usize];
        pii[r] = a.inv[b.cols[r] as usize];
        let band = (r / 3) * 3;
        na[r] = b.boxrow[band + (a.cols[r] / 3) as usize];
        nb[r] = a.boxrow[band + (b.cols[r] / 3) as usize];
    }
    // Cycle-min seed: walk pi's cycles -- exactly what production's 4-round min-doubling
    // produces (2^4 = 16 >= 9 guarantees full collapse).
    let mut lab = [u8::MAX; 9];
    for s in 0..9 {
        if lab[s] != u8::MAX {
            continue;
        }
        let mut min = s as u8;
        let mut r = pi[s] as usize;
        while r != s {
            min = min.min(r as u8);
            r = pi[r] as usize;
        }
        let mut r = s;
        loop {
            lab[r] = min;
            r = pi[r] as usize;
            if r == s {
                break;
            }
        }
    }
    rounds_to_fixpoint(&lab, &pi, &pii, &na, &nb)
}

/// Complete a placement pair (digit 1 at `a`, digit 2 at `b`) to a full grid, if possible.
fn complete(a: &Placement, b: &Placement) -> Option<[u8; 81]> {
    let mut grid = [0u8; 81];
    let (mut rows, mut cols, mut boxes) = ([0u16; 9], [0u16; 9], [0u16; 9]);
    for r in 0..9 {
        for (d, p) in [(0u16, a), (1u16, b)] {
            let c = p.cols[r] as usize;
            grid[r * 9 + c] = d as u8 + 1;
            rows[r] |= 1 << d;
            cols[c] |= 1 << d;
            boxes[(r / 3) * 3 + c / 3] |= 1 << d;
        }
    }
    fn dfs(grid: &mut [u8; 81], rows: &mut [u16; 9], cols: &mut [u16; 9], boxes: &mut [u16; 9]) -> bool {
        // MRV: branch on the most constrained empty cell.
        let (mut best_n, mut best_cell, mut best_cands) = (usize::MAX, 81, 0u16);
        for cell in 0..81 {
            if grid[cell] != 0 {
                continue;
            }
            let (r, c) = (cell / 9, cell % 9);
            let cands = !(rows[r] | cols[c] | boxes[(r / 3) * 3 + c / 3]) & 0x1FF;
            let n = cands.count_ones() as usize;
            if n == 0 {
                return false;
            }
            if n < best_n {
                (best_n, best_cell, best_cands) = (n, cell, cands);
                if n == 1 {
                    break;
                }
            }
        }
        if best_cell == 81 {
            return true;
        }
        let (r, c) = (best_cell / 9, best_cell % 9);
        let bx = (r / 3) * 3 + c / 3;
        let mut cands = best_cands;
        while cands != 0 {
            let bit = cands & cands.wrapping_neg();
            cands &= cands - 1;
            grid[best_cell] = bit.trailing_zeros() as u8 + 1;
            rows[r] |= bit;
            cols[c] |= bit;
            boxes[bx] |= bit;
            if dfs(grid, rows, cols, boxes) {
                return true;
            }
            grid[best_cell] = 0;
            rows[r] &= !bit;
            cols[c] &= !bit;
            boxes[bx] &= !bit;
        }
        false
    }
    dfs(&mut grid, &mut rows, &mut cols, &mut boxes).then_some(grid)
}

fn main() {
    let threads: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| thread::available_parallelism().map_or(1, |n| n.get()));

    let ps = placements();
    println!("placements: {} (expected {N_PLACEMENTS}), threads: {threads}", ps.len());

    struct Local {
        hist: [u64; 16],
        pairs: u64,
        examples: [Vec<(u32, u32)>; 16],
    }

    let mut locals: Vec<Local> = Vec::new();
    thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let ps = &ps;
                s.spawn(move || {
                    let mut l = Local {
                        hist: [0; 16],
                        pairs: 0,
                        examples: core::array::from_fn(|_| Vec::new()),
                    };
                    for i in (t..N_PLACEMENTS).step_by(threads) {
                        let pa = &ps[i];
                        for (dj, pb) in ps[i + 1..].iter().enumerate() {
                            if !disjoint(pa.packed, pb.packed) {
                                continue;
                            }
                            l.pairs += 1;
                            let rd = pair_rounds(pa, pb);
                            l.hist[rd] += 1;
                            if rd >= REALIZE_FROM && l.examples[rd].len() < EXAMPLE_CAP {
                                l.examples[rd].push((i as u32, (i + 1 + dj) as u32));
                            }
                        }
                    }
                    l
                })
            })
            .collect();
        locals = handles.into_iter().map(|h| h.join().unwrap()).collect();
    });

    let mut hist = [0u64; 16];
    let mut pairs = 0u64;
    let mut examples: [Vec<(u32, u32)>; 16] = core::array::from_fn(|_| Vec::new());
    for l in locals {
        pairs += l.pairs;
        for r in 0..16 {
            hist[r] += l.hist[r];
            examples[r].extend(l.examples[r].iter());
        }
    }

    let max = (0..16).rev().find(|&r| hist[r] > 0).unwrap_or(0);
    println!("disjoint pairs: {pairs}, max rounds-to-fixpoint = {max}");
    print!("histogram (rounds: count):");
    for (r, &c) in hist.iter().enumerate() {
        if c > 0 {
            print!("  {r}:{c}");
        }
    }
    println!();

    // Realizability: a level attained only by non-completable placement pairs cannot
    // occur in any actual board.
    for r in (REALIZE_FROM..16).rev() {
        if hist[r] == 0 {
            continue;
        }
        let stored = examples[r].len();
        let hit = examples[r]
            .iter()
            .find_map(|&(i, j)| complete(&ps[i as usize], &ps[j as usize]).map(|g| (i, j, g)));
        match hit {
            Some((i, j, g)) => {
                println!(
                    "rounds={r}: {} pairs, REALIZABLE in a full grid (placements {i},{j}):",
                    hist[r]
                );
                for row in 0..9 {
                    let line: String = g[row * 9..row * 9 + 9].iter().map(|d| (b'0' + d) as char).collect();
                    println!("  {line}");
                }
            }
            None => println!(
                "rounds={r}: {} pairs, none of {stored} stored examples completable{}",
                hist[r],
                if (hist[r] as usize) > stored { " (capped sample, not conclusive)" } else { " (exhaustive: level unrealizable)" },
            ),
        }
    }
}
