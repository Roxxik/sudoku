//! The last decisive layout question: does AoSoA rescue the branch/clone term?
//!
//! Pure SoA (`soa[word*P + puzzle]`) makes the sweep fast but puts one puzzle's 30
//! state words P apart — snapshotting one puzzle touches 30 cache lines, so clone
//! measured f≈0.15. AoSoA packs each warp's W puzzles' full state in one
//! contiguous 30*W-word block (`aosoa[group*30*W + word*W + lane]`), so the warp's
//! whole state is ~15 cache lines, hot in L1. The sweep reads it the same way
//! (one vector per (group,word)); the question is whether state management gets
//! cheap. Two clone modes:
//!   whole-warp : copy the contiguous 30*W block (all W lanes at once) — only valid
//!                if all lanes branch in lockstep (they don't, under refill), but
//!                the cheap bound.
//!   per-lane   : extract+restore ONE lane's 30 words (stride W within the block) —
//!                the realistic independent-DFS case.
//! Compared to scalar memcpy (the bar) and pure-SoA per-lane (today's f≈0.15).
//!
//! Usage: cargo run --release -p generator-lab --example aosoabench -- [--puzzles P=8192] [--iters I=4000]

#![feature(portable_simd)]

use std::simd::Simd;
use std::time::Instant;

const W: usize = 8;
const SW: usize = 30;
type V = Simd<u32, W>;
type Vi = Simd<usize, W>;

fn main() {
    let mut puzzles = 8192usize;
    let mut iters = 4000usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--puzzles" => puzzles = it.next().and_then(|s| s.parse().ok()).unwrap_or(puzzles),
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            _ => {}
        }
    }
    puzzles -= puzzles % W;
    let p = puzzles;
    let groups = p / W;
    let work = (p * iters) as f64;
    let mut sink = 0u64;

    // ---- scalar memcpy (the bar) ----
    let aos: Vec<[u32; SW]> = (0..p).map(|i| core::array::from_fn(|w| (i * 7 + w) as u32)).collect();
    let mut frame = [0u32; SW];
    let t = Instant::now();
    for _ in 0..iters {
        for i in 0..p {
            frame = aos[i];
            sink = sink.wrapping_add(frame[i % SW] as u64);
        }
    }
    let ns_scalar = t.elapsed().as_secs_f64() * 1e9 / work;
    std::hint::black_box(&frame);

    // ---- pure-SoA per-lane (today): soa[w*p + i], gather one lane's 30 words ----
    let soa: Vec<u32> = (0..SW * p).map(|x| x as u32).collect();
    let mut stash = vec![0u32; SW * p];
    let t = Instant::now();
    for _ in 0..iters {
        for base in (0..p).step_by(W) {
            for w in 0..SW {
                // one lane = soa[w*p + base + j]; the 30 words of lane j are p apart.
                V::from_slice(&soa[w * p + base..w * p + base + W])
                    .copy_to_slice(&mut stash[w * p + base..w * p + base + W]);
            }
            sink = sink.wrapping_add(stash[base] as u64);
        }
    }
    let ns_soa = t.elapsed().as_secs_f64() * 1e9 / work;
    std::hint::black_box(&stash);

    // ---- AoSoA whole-warp: contiguous 30*W block copy ----
    let aosoa: Vec<u32> = (0..SW * W * groups).map(|x| x as u32).collect();
    let mut wframe = vec![0u32; SW * W * groups];
    let t = Instant::now();
    for _ in 0..iters {
        for g in 0..groups {
            let blk = g * SW * W;
            wframe[blk..blk + SW * W].copy_from_slice(&aosoa[blk..blk + SW * W]);
            sink = sink.wrapping_add(wframe[blk] as u64);
        }
    }
    let ns_aw = t.elapsed().as_secs_f64() * 1e9 / work; // per puzzle (block / W)
    std::hint::black_box(&wframe);

    // ---- AoSoA per-lane: extract+restore one lane (stride W within the block) ----
    // aosoa[g*SW*W + w*W + lane]; one lane's 30 words are W apart but L1-hot.
    let mut aosoa2: Vec<u32> = (0..SW * W * groups).map(|x| x as u32).collect();
    let mut lane_stash = [0u32; SW];
    let t = Instant::now();
    for _ in 0..iters {
        for g in 0..groups {
            for lane in 0..W {
                let base = g * SW * W + lane;
                for w in 0..SW {
                    lane_stash[w] = aosoa2[base + w * W];
                }
                // (child would mutate; here just restore to keep it honest)
                for w in 0..SW {
                    aosoa2[base + w * W] = lane_stash[w];
                }
                sink = sink.wrapping_add(lane_stash[0] as u64);
            }
        }
    }
    let ns_al = t.elapsed().as_secs_f64() * 1e9 / work;

    // ---- AoSoA per-lane via SIMD gather/scatter (stride-W indices) ----
    let aosoa3: Vec<u32> = (0..SW * W * groups).map(|x| x as u32).collect();
    let mut g_stash = vec![0u32; SW * groups * W];
    let t = Instant::now();
    for _ in 0..iters {
        for g in 0..groups {
            // gather lane-strided: for each of W lanes pick its 30 words. Do it as W
            // separate lanes is scalar; instead gather across the 8 lanes for word w.
            for w in 0..SW {
                let idx = Vi::from_array(core::array::from_fn(|lane| g * SW * W + w * W + lane));
                let v = V::gather_or_default(&aosoa3, idx);
                v.copy_to_slice(&mut g_stash[(g * SW + w) * W..(g * SW + w) * W + W]);
            }
            sink = sink.wrapping_add(g_stash[g * SW * W] as u64);
        }
    }
    let ns_ag = t.elapsed().as_secs_f64() * 1e9 / work;
    std::hint::black_box(&g_stash);

    println!("aosoabench: {p} puzzles x {iters} iters, W={W}, state={SW} words/puzzle\n");
    println!("  {:<22} {:>9.3} ns/clone   f={:.2}x", "scalar-memcpy (bar)", ns_scalar, 1.0);
    println!("  {:<22} {:>9.3} ns/clone   f={:.2}x", "SoA per-lane (today)", ns_soa, ns_scalar / ns_soa);
    println!("  {:<22} {:>9.3} ns/clone   f={:.2}x", "AoSoA whole-warp", ns_aw, ns_scalar / ns_aw);
    println!("  {:<22} {:>9.3} ns/clone   f={:.2}x", "AoSoA per-lane (scalar)", ns_al, ns_scalar / ns_al);
    println!("  {:<22} {:>9.3} ns/clone   f={:.2}x", "AoSoA per-lane (gather)", ns_ag, ns_scalar / ns_ag);
    println!("\n  f = scalar/variant; >1 packs, <1 regresses. AoSoA whole-warp is the");
    println!("  lockstep bound; per-lane is the realistic independent-DFS cost.");
    std::hint::black_box(sink);
}
