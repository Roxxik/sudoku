//! Can `place_singles` (the 20%-of-prober naked-single wave) be made UNIFORM and
//! packed? It loops all 9 digits explicitly, so unlike single-cell `place(cell,d)`
//! it has no per-lane-varying-digit problem; and the per-cell peer-accumulation
//! has a uniform smear reformulation. The one SIMT cost the per-digit `placesmear`
//! missed: the vector path must process ALL 9 digits for ALL lanes (no per-lane
//! `continue` on an empty group), while scalar skips the empties (~1-3 non-empty
//! digits per wave). This benches the FULL `place_singles` both ways to see if it
//! still packs despite that overhead.
//!
//!   scalar : per puzzle, `for d` skipping empty groups, per-cell peer loop (the
//!            shipped algorithm).
//!   simt   : W puzzles, `for d in 0..9` unconditionally, smear peers per lane,
//!            masked AND-NOT — fully uniform across lanes.
//!
//! Usage: cargo run --release -p generator-lab --example psbench -- [--puzzles P=8192] [--iters I=1500] [--singles N=2]

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
#[inline(always)]
fn rm_cell(lane: usize, bit: u32) -> usize {
    let b = bit as usize;
    (3 * lane + b / 9) * 9 + b % 9
}

const BOX_CELLS: [u32; 3] = {
    let mut t = [0u32; 3];
    let mut k = 0;
    while k < 3 {
        let c3 = 0b111u32 << (3 * k);
        t[k] = c3 | (c3 << 9) | (c3 << 18);
        k += 1;
    }
    t
};

fn peer_table() -> Vec<[u32; 3]> {
    let mut t = vec![[0u32; 3]; CELLS];
    for c in 0..CELLS {
        let (r, col) = (c / 9, c % 9);
        let (br, bc) = (r / 3 * 3, col / 3 * 3);
        for q in 0..CELLS {
            if q != c && (q / 9 == r || q % 9 == col || (q / 9 / 3 * 3 == br && q % 9 / 3 * 3 == bc)) {
                t[c][rm_lane(q)] |= 1u32 << rm_bit(q);
            }
        }
    }
    t
}

/// One puzzle: nine row-major digit boards + the naked-single wave to place.
#[derive(Clone)]
struct Puz {
    r: [[u32; 3]; 9],
    singles: [u32; 3],
}

fn gen_puz(p: usize, nsing: usize) -> Vec<Puz> {
    let mut s: u64 = 0x1357_9bdf_2468_ace0;
    let mut nx = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut m27 = move || (nx() as u32 & 0x07ff_ffff) & (nx() as u32 | 0x000f_ffff);
    (0..p)
        .map(|_| {
            // random candidate boards
            let mut r: [[u32; 3]; 9] = core::array::from_fn(|_| [m27(), m27(), m27()]);
            let mut singles = [0u32; 3];
            // place `nsing` naked singles: a cell belongs to exactly one digit.
            for _ in 0..nsing {
                let cell = (nx() % CELLS as u64) as usize;
                let d = (nx() % 9) as usize;
                let (l, bit) = (rm_lane(cell), rm_bit(cell));
                for dd in 0..9 {
                    r[dd][l] &= !(1 << bit); // clear cell from all digits
                }
                r[d][l] |= 1 << bit; // exactly its single digit
                singles[l] |= 1 << bit;
            }
            Puz { r, singles }
        })
        .collect()
}

#[inline(always)]
fn smear_v(group: [V; 3]) -> [V; 3] {
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
            rowpeer |= chunk.simd_ne(V::splat(0)).select(V::splat(0x1FF << (9 * i)), V::splat(0));
        }
        let mut boxpeer = V::splat(0);
        for k in 0..3 {
            let bp = V::splat(BOX_CELLS[k]);
            boxpeer |= (g & bp).simd_ne(V::splat(0)).select(bp, V::splat(0));
        }
        pr[b] = rowpeer | boxpeer | colpeer;
    }
    pr
}

fn main() {
    let mut puzzles = 8192usize;
    let mut iters = 1500usize;
    let mut nsing = 2usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--puzzles" => puzzles = it.next().and_then(|s| s.parse().ok()).unwrap_or(puzzles),
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            "--singles" => nsing = it.next().and_then(|s| s.parse().ok()).unwrap_or(nsing),
            _ => {}
        }
    }
    puzzles -= puzzles % W;
    let p = puzzles;
    let peer = peer_table();
    let data = gen_puz(p, nsing);
    let work = (p * iters) as f64;

    // ---- scalar place_singles (shipped algorithm, skips empty digits) ----
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for puz in &data {
            let mut r = puz.r;
            for d in 0..9 {
                let group = [puz.singles[0] & r[d][0], puz.singles[1] & r[d][1], puz.singles[2] & r[d][2]];
                if group[0] | group[1] | group[2] == 0 {
                    continue;
                }
                let mut peers = [0u32; 3];
                for lane in 0..3 {
                    let mut g = group[lane];
                    while g != 0 {
                        let cell = rm_cell(lane, g.trailing_zeros());
                        g &= g - 1;
                        peers[0] |= peer[cell][0];
                        peers[1] |= peer[cell][1];
                        peers[2] |= peer[cell][2];
                    }
                }
                // also clear self, so the result matches the smear (whose peer set
                // is peers_percell ∪ group). Harmless: those cells are now solved.
                for b in 0..3 {
                    r[d][b] &= !(peers[b] | group[b]);
                }
            }
            // fold the FULL result (all 27 words) so nothing is dead-stripped.
            let mut x = 0u32;
            for d in 0..9 {
                for b in 0..3 {
                    x ^= r[d][b];
                }
            }
            chk ^= x as u64;
        }
    }
    let ns_scalar = t.elapsed().as_secs_f64() * 1e9 / work;
    println!("psbench: {p} puzzles x {iters} iters, W={W}, {nsing} singles/wave\n");
    println!("  {:<14} {:>9.3} ns/wave   (chk {:#018x})", "scalar", ns_scalar, chk);

    // ---- SIMT place_singles (all 9 digits, smear, masked, uniform) ----
    // SoA: rb[d][b][puzzle], sb[b][puzzle].
    let rb: Vec<Vec<Vec<u32>>> =
        (0..9).map(|d| (0..3).map(|b| data.iter().map(|x| x.r[d][b]).collect()).collect()).collect();
    let sb: Vec<Vec<u32>> = (0..3).map(|b| data.iter().map(|x| x.singles[b]).collect()).collect();
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for base in (0..p).step_by(W) {
            let sing = [
                V::from_slice(&sb[0][base..base + W]),
                V::from_slice(&sb[1][base..base + W]),
                V::from_slice(&sb[2][base..base + W]),
            ];
            let mut acc = V::splat(0);
            for d in 0..9 {
                let rd = [
                    V::from_slice(&rb[d][0][base..base + W]),
                    V::from_slice(&rb[d][1][base..base + W]),
                    V::from_slice(&rb[d][2][base..base + W]),
                ];
                let group = [sing[0] & rd[0], sing[1] & rd[1], sing[2] & rd[2]];
                let peers = smear_v(group);
                // r[d] &= !peers; empty-group lanes get peers=0 => no-op (no per-lane branch).
                // smear's peer set already includes self, matching the scalar path.
                acc ^= (rd[0] & !peers[0]) ^ (rd[1] & !peers[1]) ^ (rd[2] & !peers[2]);
            }
            chk ^= acc.reduce_xor() as u64;
        }
    }
    let ns_simt = t.elapsed().as_secs_f64() * 1e9 / work;
    println!("  {:<14} {:>9.3} ns/wave   (chk {:#018x})", "simt-smear", ns_simt, chk);

    println!("\n  packing speedup (scalar / simt): {:.2}x", ns_scalar / ns_simt);
    println!("  [full place_singles incl the 'all 9 digits for all lanes' SIMT overhead;");
    println!("   >2.67 beats the 3-band baseline, <1 regresses]");
}
