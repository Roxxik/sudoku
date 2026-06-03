//! Prototypes trail-based branching vs clone-based — the swing between ~1.4x and
//! ~1.8x in the cost model. This isolates the branch-MANAGEMENT cost only (the
//! state mutation itself is propagation work, counted under place_singles /
//! band_update, and is identical either way).
//!
//! Per branch-child the manager must let the child explore and then recover the
//! parent state:
//!   clone : copy the whole 30-word state to a child frame up front; parent is
//!           untouched, so backtrack is free. Scalar = a 120-byte memcpy; SoA =
//!           30 contiguous vector ld+st, but across 30 cache lines (the p-stride).
//!   trail : mutate in place, logging each of the K changed words as (idx,old) at
//!           a SHARED warp depth (contiguous vector store, no scatter), then undo
//!           by reverse-replaying — K scatters of old back to the per-lane word.
//!
//! Trail wins when K (words changed before backtrack) is small AND the undo
//! scatter is cheaper than copying 30 words. The reported f is vs scalar-clone
//! (today's prober) — the value the cost model's clone term needs (was 0.2 naive).
//!
//! Usage: cargo run --release -p generator-lab --example trailbench -- [--puzzles P=8192] [--iters I=4000] [--k K=8]

#![feature(portable_simd)]

use std::simd::Simd;
use std::time::Instant;

const W: usize = 8;
const SW: usize = 30; // state words per puzzle
type V = Simd<u32, W>;
type Vi = Simd<usize, W>;

fn main() {
    let mut puzzles = 8192usize;
    let mut iters = 4000usize;
    let mut k = 8usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--puzzles" => puzzles = it.next().and_then(|s| s.parse().ok()).unwrap_or(puzzles),
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            "--k" => k = it.next().and_then(|s| s.parse().ok()).unwrap_or(k),
            _ => {}
        }
    }
    puzzles -= puzzles % W;
    let p = puzzles;

    let mut s: u64 = 0xc0ff_ee00_1234_abcd;
    let mut nx = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    // which word each (puzzle, change-slot) touches.
    let touch: Vec<usize> = (0..p * k.max(1)).map(|_| (nx() % SW as u64) as usize).collect();
    let work = (p * iters) as f64;
    let mut sink = 0u64;

    // ===== scalar-clone: make a child = 120-byte memcpy. =====
    let aos: Vec<[u32; SW]> = (0..p).map(|i| core::array::from_fn(|w| (i * 7 + w) as u32)).collect();
    let mut child = [0u32; SW];
    let t = Instant::now();
    for _ in 0..iters {
        for i in 0..p {
            child = aos[i]; // clone parent into child frame
            sink = sink.wrapping_add(child[i % SW] as u64);
        }
    }
    let ns_sclone = t.elapsed().as_secs_f64() * 1e9 / work;
    std::hint::black_box(&child);

    // ===== scalar-trail: log K olds, undo K. =====
    let mut aos2: Vec<[u32; SW]> = (0..p).map(|i| core::array::from_fn(|w| (i * 3 + w) as u32)).collect();
    let mut ti = [0usize; 64];
    let mut to = [0u32; 64];
    let t = Instant::now();
    for _ in 0..iters {
        for i in 0..p {
            for s in 0..k {
                let w = touch[i * k + s];
                ti[s] = w;
                to[s] = aos2[i][w];
                aos2[i][w] = aos2[i][w].wrapping_add(1);
            }
            for s in (0..k).rev() {
                aos2[i][ti[s]] = to[s];
            }
            sink = sink.wrapping_add(aos2[i][0] as u64);
        }
    }
    let ns_strail = t.elapsed().as_secs_f64() * 1e9 / work;

    // SoA state soa[w*p + i].
    let mk = || -> Vec<u32> { (0..SW * p).map(|x| x as u32).collect() };

    // ===== simt-clone: copy 30 SoA words warp->frame (contiguous vec ld/st). =====
    let soa = mk();
    let mut frame = vec![0u32; SW * W * (p / W)];
    let t = Instant::now();
    for _ in 0..iters {
        for (g, base) in (0..p).step_by(W).enumerate() {
            for w in 0..SW {
                V::from_slice(&soa[w * p + base..w * p + base + W])
                    .copy_to_slice(&mut frame[(g * SW + w) * W..(g * SW + w) * W + W]);
            }
        }
    }
    let ns_vclone = t.elapsed().as_secs_f64() * 1e9 / work;
    std::hint::black_box(&frame);

    // ===== simt-trail: log K (idx,old) contiguous; undo = K scatters. =====
    // (reading `old` is free during the real mutation, so the log is just stores;
    //  here we gather once to have a realistic `old`, then store + scatter-undo.)
    let mut soa = mk();
    let mut tidx = vec![Vi::splat(0); 64];
    let mut told = vec![V::splat(0); 64];
    let t = Instant::now();
    for _ in 0..iters {
        for base in (0..p).step_by(W) {
            for s in 0..k {
                let idx = Vi::from_array(core::array::from_fn(|j| touch[(base + j) * k + s] * p + base + j));
                tidx[s] = idx; // contiguous store (shared warp depth)
                told[s] = V::gather_or_default(&soa, idx); // old (free in real code)
            }
            for s in (0..k).rev() {
                told[s].scatter(&mut soa, tidx[s]); // undo: scatter old back
            }
            sink = sink.wrapping_add(soa[base] as u64);
        }
    }
    let ns_vtrail = t.elapsed().as_secs_f64() * 1e9 / work;

    println!("trailbench: {p} puzzles x {iters} iters, W={W}, k={k} changed words/branch");
    println!("  (branch-MANAGEMENT only; mutation is propagation work, counted elsewhere)\n");
    println!("  {:<14} {:>9.3} ns/branch", "scalar-clone", ns_sclone);
    println!("  {:<14} {:>9.3} ns/branch", "scalar-trail", ns_strail);
    println!("  {:<14} {:>9.3} ns/branch", "simt-clone", ns_vclone);
    println!("  {:<14} {:>9.3} ns/branch", "simt-trail", ns_vtrail);
    let scalar_best = ns_sclone.min(ns_strail);
    let simt_best = ns_vclone.min(ns_vtrail);
    println!("\n  f vs scalar-clone (today):  simt-clone {:.2}x  simt-trail {:.2}x  simt-best {:.2}x",
        ns_sclone / ns_vclone, ns_sclone / ns_vtrail, ns_sclone / simt_best);
    println!("  f vs scalar-best:           simt-best {:.2}x", scalar_best / simt_best);
    std::hint::black_box(sink);
}
