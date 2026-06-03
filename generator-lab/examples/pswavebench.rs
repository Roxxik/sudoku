//! The swing factor: a FAITHFUL `place_singles` per-digit body. This is 20% of
//! prober self-time and the part I had only idealized. Real `place_singles`
//! accumulates the placed wave's peer mask with a VARIABLE-LENGTH per-cell loop:
//!
//!   for each set bit (cell) of the digit's group: peers |= peer_mask[cell]
//!
//! then clears `peers` from the digit board. In scalar AoS the loop runs the
//! puzzle's own popcount. Packed across W lanes it must run to the MAX popcount in
//! the warp (lanes with fewer singles idle), and each step GATHERS a peer mask by
//! per-lane cell — the two SIMT taxes (loop divergence + gather) stacked. This
//! prices both against the scalar baseline on identical waves.
//!
//! Group sizes are drawn skewed small (naked-single waves are typically 1-3
//! cells); `--avg` shifts the mean to probe sensitivity.
//!
//! Usage: cargo run --release -p generator-lab --example pswavebench -- [--puzzles P=8192] [--iters I=1500] [--avg A=1.8]

#![feature(portable_simd)]

use std::simd::cmp::SimdPartialOrd;
use std::simd::num::SimdUint;
use std::simd::{Select, Simd};
use std::time::Instant;

const W: usize = 8;
const CELLS: usize = 81;
type V = Simd<u32, W>;
type Vi = Simd<usize, W>;

const fn rm_lane(c: usize) -> usize {
    (c / 9) / 3
}
const fn rm_bit(c: usize) -> usize {
    ((c / 9) % 3) * 9 + c % 9
}

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

fn main() {
    let mut puzzles = 8192usize;
    let mut iters = 1500usize;
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

    // Per-puzzle wave: a small list of distinct cells. Geometric-ish size.
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut nx = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let maxg = 6usize;
    let waves: Vec<Vec<usize>> = (0..p)
        .map(|_| {
            // size: at least 1, grow while a coin keeps landing under avg-derived p.
            let keep = 1.0 - 1.0 / avg.max(1.0001);
            let mut n = 1usize;
            while n < maxg && (nx() as f64 / u64::MAX as f64) < keep {
                n += 1;
            }
            (0..n).map(|_| (nx() % CELLS as u64) as usize).collect()
        })
        .collect();
    let mean: f64 = waves.iter().map(|w| w.len() as f64).sum::<f64>() / p as f64;
    let work = (p * iters) as f64;

    // ---- scalar AoS ----
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for i in 0..p {
            let mut pr = [0u32; 3];
            for &c in &waves[i] {
                pr[0] |= peer[c][0];
                pr[1] |= peer[c][1];
                pr[2] |= peer[c][2];
            }
            chk = chk.wrapping_add(pr[0] as u64 ^ (pr[1] as u64) << 1 ^ (pr[2] as u64) << 2);
        }
    }
    let ns_scalar = t.elapsed().as_secs_f64() * 1e9 / work;
    println!("pswavebench: {p} puzzles x {iters} iters, W={W}, mean wave {mean:.2} cells\n");
    println!("  {:<14} {:>9.3} ns/wave   (chk {:#018x})", "scalar-aos", ns_scalar, chk);

    // ---- SIMT: run to max popcount, gather peer mask per lane per step ----
    // Pad each lane's wave to maxg with a sentinel cell whose peer mask is 0.
    let sentinel = CELLS; // index into a padded peer table row of zeros
    let mut peerpad = peer.clone();
    peerpad.push([0, 0, 0]); // row `sentinel`
    let lens: Vec<u32> = waves.iter().map(|w| w.len() as u32).collect();
    let mut cellgrid = vec![sentinel; p * maxg]; // cellgrid[i*maxg + k]
    for i in 0..p {
        for (k, &c) in waves[i].iter().enumerate() {
            cellgrid[i * maxg + k] = c;
        }
    }
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for base in (0..p).step_by(W) {
            let lenv = V::from_array(core::array::from_fn(|j| lens[base + j]));
            let mut pr = [V::splat(0); 3];
            let kmax = lenv.reduce_max(); // run to the warp's slowest lane, not maxg
            for k in 0..kmax {
                let active = V::splat(k).simd_lt(lenv); // lanes still having a cell
                let idx = Vi::from_array(core::array::from_fn(|j| cellgrid[(base + j) * maxg + k as usize]));
                // gather the three band words of peer[cell] per lane.
                for b in 0..3 {
                    let g = V::from_array(core::array::from_fn(|j| peerpad[idx[j]][b]));
                    pr[b] = active.select(pr[b] | g, pr[b]);
                }
            }
            chk = chk.wrapping_add(pr[0].reduce_sum() as u64 ^ (pr[1].reduce_sum() as u64) << 1);
        }
    }
    let ns_simt = t.elapsed().as_secs_f64() * 1e9 / work;
    println!("  {:<14} {:>9.3} ns/wave   (chk {:#018x})", "simt-gather", ns_simt, chk);

    println!("\n  packing speedup (scalar / simt): {:.2}x", ns_scalar / ns_simt);
    println!("  [place_singles is 20% of prober self-time; this is its true packing factor]");
}
