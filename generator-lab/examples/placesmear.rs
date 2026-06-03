//! Reformulating place_singles' peer accumulation as a UNIFORM band op (no
//! per-cell loop, no gather) — the lever that could move the 20%-of-prober
//! place_singles term off its f≈0.9 ceiling.
//!
//! For digit d's placed cells `group` (a 27-bit-per-band mask), the cells that
//! lose d are the union of group's rows, columns, and boxes. Each decomposes:
//!   row peers : smear each 9-bit row chunk to 0x1FF if nonzero        (row-major in-lane)
//!   box peers : expand each touched box to its 9 cells                (row-major in-lane)
//!   col peers : fold group to a 9-bit column-occupancy, broadcast it
//!               to all 3 rows of all 3 bands (occ * 0x40201)          (NO transpose)
//! The result is a superset of the true peer mask by exactly the group cells
//! themselves (self), which is harmless: those cells are marked solved the same
//! step, so the stale candidate is gated out everywhere it is read.
//!
//! This validates the band formula against the per-cell peer-mask union, then
//! benchmarks scalar-per-cell vs scalar-smear vs SIMT-smear (W=8).
//!
//! Usage: cargo run --release -p generator-lab --example placesmear -- [--puzzles P=8192] [--iters I=2000] [--avg A=1.8]

#![feature(portable_simd)]

use std::simd::cmp::SimdPartialEq;
use std::simd::num::SimdUint;
use std::simd::{Select, Simd};
use std::time::Instant;

const W: usize = 8;
const CELLS: usize = 81;
type V = Simd<u32, W>;

const fn rm_lane(c: usize) -> usize {
    (c / 9) / 3
}
const fn rm_bit(c: usize) -> usize {
    ((c / 9) % 3) * 9 + c % 9
}

/// peer_mask_r[cell] = the 20 peers of cell as 3 row-major band words (self excl).
fn peer_table() -> Vec<[u32; 3]> {
    let mut peer = vec![[0u32; 3]; CELLS];
    for c in 0..CELLS {
        let (r, col) = (c / 9, c % 9);
        let (br, bc) = (r / 3 * 3, col / 3 * 3);
        for q in 0..CELLS {
            if q != c && (q / 9 == r || q % 9 == col || (q / 9 / 3 * 3 == br && q % 9 / 3 * 3 == bc)) {
                peer[c][rm_lane(q)] |= 1u32 << rm_bit(q);
            }
        }
    }
    peer
}

/// Per-band box patterns: BOXPAT[k] = the 9 cells of box-column k within a band.
const fn boxpats() -> [u32; 3] {
    let mut t = [0u32; 3];
    let mut k = 0;
    while k < 3 {
        let col3 = 0b111u32 << (3 * k);
        t[k] = col3 | (col3 << 9) | (col3 << 18);
        k += 1;
    }
    t
}
const BOXPAT: [u32; 3] = boxpats();

/// SCALAR per-cell reference: union of peer masks over the cells set in `group`.
#[inline]
fn peers_percell(group: [u32; 3], peer: &[[u32; 3]]) -> [u32; 3] {
    let mut pr = [0u32; 3];
    for b in 0..3 {
        let mut g = group[b];
        while g != 0 {
            let bit = g.trailing_zeros();
            g &= g - 1;
            let cell = (3 * b + bit as usize / 9) * 9 + bit as usize % 9;
            pr[0] |= peer[cell][0];
            pr[1] |= peer[cell][1];
            pr[2] |= peer[cell][2];
        }
    }
    pr
}

/// SCALAR band-formula: row-smear | box-expand | col-broadcast. Superset by self.
#[inline]
fn peers_smear(group: [u32; 3]) -> [u32; 3] {
    // column occupancy folded across all rows and bands (cols span all 3 bands).
    let mut col_occ = 0u32;
    for b in 0..3 {
        col_occ |= (group[b] & 0x1FF) | ((group[b] >> 9) & 0x1FF) | ((group[b] >> 18) & 0x1FF);
    }
    let colpeer = col_occ | (col_occ << 9) | (col_occ << 18);
    let mut pr = [0u32; 3];
    for b in 0..3 {
        let g = group[b];
        // row peers: each 9-bit chunk -> 0x1FF if nonzero.
        let mut rowpeer = 0u32;
        for i in 0..3 {
            if (g >> (9 * i)) & 0x1FF != 0 {
                rowpeer |= 0x1FF << (9 * i);
            }
        }
        // box peers: each touched box -> its 9 cells.
        let mut boxpeer = 0u32;
        for k in 0..3 {
            if g & BOXPAT[k] != 0 {
                boxpeer |= BOXPAT[k];
            }
        }
        pr[b] = rowpeer | boxpeer | colpeer;
    }
    pr
}

/// SIMD band-formula across W puzzles. All uniform vector ops, no gather.
#[inline]
fn peers_smear_v(group: [V; 3]) -> [V; 3] {
    let m9 = V::splat(0x1FF);
    let mut col_occ = V::splat(0);
    for b in 0..3 {
        col_occ |= (group[b] & m9) | ((group[b] >> V::splat(9)) & m9) | ((group[b] >> V::splat(18)) & m9);
    }
    let colpeer = col_occ | (col_occ << V::splat(9)) | (col_occ << V::splat(18));
    let mut pr = [V::splat(0); 3];
    for b in 0..3 {
        let g = group[b];
        let mut rowpeer = V::splat(0);
        for i in 0..3u32 {
            let chunk = (g >> V::splat(9 * i)) & m9;
            let on = chunk.simd_ne(V::splat(0)).select(V::splat(0x1FF << (9 * i)), V::splat(0));
            rowpeer |= on;
        }
        let mut boxpeer = V::splat(0);
        for k in 0..3 {
            let bp = V::splat(BOXPAT[k]);
            let on = (g & bp).simd_ne(V::splat(0)).select(bp, V::splat(0));
            boxpeer |= on;
        }
        pr[b] = rowpeer | boxpeer | colpeer;
    }
    pr
}

fn main() {
    let mut puzzles = 8192usize;
    let mut iters = 2000usize;
    let mut avg = 1.8f64;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--puzzles" => puzzles = it.next().and_then(|s| s.parse().ok()).unwrap_or(puzzles),
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            "--avg" => avg = it.next().and_then(|s| s.parse().ok()).unwrap_or(avg),
            _ => {}
        }
    }
    puzzles -= puzzles % W;
    let p = puzzles;
    let peer = peer_table();

    // Build conflict-free groups (no two cells share a unit) so the per-cell union
    // excludes all group cells and equals smear & !group — the validation identity.
    let mut s: u64 = 0x2468_ace0_1357_9bdf;
    let mut nx = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut groups: Vec<[u32; 3]> = Vec::with_capacity(p);
    for _ in 0..p {
        let keep = 1.0 - 1.0 / avg.max(1.0001);
        let mut want = 1;
        while want < 6 && (nx() as f64 / u64::MAX as f64) < keep {
            want += 1;
        }
        let mut g = [0u32; 3];
        let mut placed = 0;
        let mut tries = 0;
        while placed < want && tries < 40 {
            tries += 1;
            let c = (nx() % CELLS as u64) as usize;
            // reject if it shares a unit with an already-chosen cell.
            let pr = peers_percell(g, &peer);
            let b = rm_lane(c);
            let bit = 1u32 << rm_bit(c);
            if g[b] & bit == 0 && pr[b] & bit == 0 {
                g[b] |= bit;
                placed += 1;
            }
        }
        groups.push(g);
    }

    // ---- validate: smear & !group == per-cell union, for every group. ----
    let mut bad = 0;
    for g in &groups {
        let pc = peers_percell(*g, &peer);
        let sm = peers_smear(*g);
        for b in 0..3 {
            if (sm[b] & !g[b]) != pc[b] {
                bad += 1;
                break;
            }
        }
    }
    println!("placesmear: {p} groups, mean {:.2} cells/group", groups.iter().map(|g| (g[0].count_ones()+g[1].count_ones()+g[2].count_ones()) as f64).sum::<f64>()/p as f64);
    println!("  VALIDATION: {} / {p} groups mismatched (smear&!group vs per-cell union)\n", bad);
    if bad != 0 {
        println!("  reformulation is WRONG — aborting bench");
        return;
    }

    let work = (p * iters) as f64;
    // scalar per-cell
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for g in &groups {
            let pr = peers_percell(*g, &peer);
            chk = chk.wrapping_add(pr[0] as u64 ^ (pr[1] as u64) << 1 ^ (pr[2] as u64) << 2);
        }
    }
    let ns_pc = t.elapsed().as_secs_f64() * 1e9 / work;
    println!("  {:<18} {:>9.3} ns/group   (chk {:#018x})", "scalar-per-cell", ns_pc, chk);

    // scalar smear
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for g in &groups {
            let pr = peers_smear(*g);
            chk = chk.wrapping_add((pr[0] & !g[0]) as u64 ^ ((pr[1] & !g[1]) as u64) << 1 ^ ((pr[2] & !g[2]) as u64) << 2);
        }
    }
    let ns_sm = t.elapsed().as_secs_f64() * 1e9 / work;
    println!("  {:<18} {:>9.3} ns/group   (chk {:#018x})", "scalar-smear", ns_sm, chk);

    // simt smear
    let bands: [Vec<u32>; 3] = core::array::from_fn(|b| groups.iter().map(|g| g[b]).collect());
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for base in (0..p).step_by(W) {
            let g = [
                V::from_slice(&bands[0][base..base + W]),
                V::from_slice(&bands[1][base..base + W]),
                V::from_slice(&bands[2][base..base + W]),
            ];
            let pr = peers_smear_v(g);
            chk = chk.wrapping_add((pr[0] & !g[0]).reduce_xor() as u64);
        }
    }
    let ns_smv = t.elapsed().as_secs_f64() * 1e9 / work;
    println!("  {:<18} {:>9.3} ns/group   (chk {:#018x})", "simt-smear", ns_smv, chk);

    println!("\n  scalar reformulation (per-cell / smear): {:.2}x", ns_pc / ns_sm);
    println!("  SIMT packing (scalar-per-cell / simt-smear): {:.2}x   <- the new place_singles f", ns_pc / ns_smv);
    println!("  SIMT vs scalar-smear: {:.2}x", ns_sm / ns_smv);
}
