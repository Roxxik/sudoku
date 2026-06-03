//! Validates the packing factor of the LAST unmeasured prober term: the
//! clone-on-branch. `solve_first` clones the whole BitBoard at each branch point
//! (1.48/query) and restores it for the next child. In scalar AoS that's a flat
//! ~120-byte memcpy of one puzzle's state (single-layout: 9 digits x 3 bands + 3
//! unsolved = 30 u32). In a refill warp every lane runs an INDEPENDENT DFS, so a
//! branching lane must snapshot just ITS lane out of 30 SoA vectors to a per-lane
//! stack and later restore it — a masked per-lane scatter, then gather. Same
//! bytes, but scatter/gather addressing vs linear memcpy.
//!
//! Two strategies are measured against the scalar baseline:
//!   simt-scatter : true per-lane independent stacks. Save = 30 masked scatters of
//!                  the current SoA words to lane-private stack frames; restore =
//!                  30 masked gathers. Models refill where lanes branch at unequal
//!                  depths (the realistic case).
//!   simt-whole   : snapshot the WHOLE warp frame (all W lanes) with contiguous
//!                  vector ld/st, no scatter. Cheap, but only valid if every lane
//!                  branches in lockstep at a shared depth — the upper bound a
//!                  refill scheduler can NEVER quite reach, shown for contrast.
//!
//! `--active F` is the fraction of lanes that branch on a given step (branches are
//! ~1 per node, so a warp step rarely has all 8 branching at once).
//!
//! Usage: cargo run --release -p generator-lab --example clonebench -- [--puzzles P=8192] [--iters I=2000] [--active F=0.4]

#![feature(portable_simd)]

use std::simd::cmp::SimdPartialEq;
use std::simd::{Select, Simd};
use std::time::Instant;

const W: usize = 8;
const SW: usize = 30; // state words per puzzle (single-layout BitBoard)
type V = Simd<u32, W>;
type Vi = Simd<usize, W>;

fn fill(n: usize, seed: u64) -> Vec<u32> {
    let mut s = seed;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s as u32 & 0x07ff_ffff
    };
    (0..n).map(|_| next()).collect()
}

fn main() {
    let mut puzzles = 8192usize;
    let mut iters = 2000usize;
    let mut active = 0.4f64;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--puzzles" => puzzles = it.next().and_then(|s| s.parse().ok()).unwrap_or(puzzles),
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            "--active" => active = it.next().and_then(|s| s.parse().ok()).unwrap_or(active),
            _ => {}
        }
    }
    puzzles -= puzzles % W;
    let p = puzzles;

    let mut s: u64 = 0x5151_2727_9393_bdbd;
    let mut nb = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let act: Vec<bool> = (0..p).map(|_| (nb() as f64 / u64::MAX as f64) < active).collect();
    let branches = (0..iters).map(|_| p).sum::<usize>() as f64; // round trips attempted (incl masked)

    // ---- scalar AoS: one [u32;SW] per puzzle, memcpy save + restore. ----
    let mut aos: Vec<[u32; SW]> = (0..p)
        .map(|i| core::array::from_fn(|w| (i + w) as u32))
        .collect();
    let mut stack: Vec<[u32; SW]> = vec![[0u32; SW]; p];
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for i in 0..p {
            if !act[i] {
                continue;
            }
            stack[i] = aos[i]; // save (clone)
            aos[i][0] = aos[i][0].wrapping_add(1); // child mutates
            aos[i] = stack[i]; // restore for next child
            chk = chk.wrapping_add(aos[i][0] as u64);
        }
    }
    let ns_scalar = t.elapsed().as_secs_f64() * 1e9 / branches;
    println!("clonebench: {p} puzzles x {iters} iters, W={W}, active={active}, state={SW} words\n");
    println!("  {:<14} {:>9.3} ns/branch   (chk {:#018x})", "scalar-aos", ns_scalar, chk);

    // ---- SoA storage: soa[w*p + i] (word w contiguous across puzzles). ----
    // simt-scatter: per-lane stack frames, masked scatter save + masked gather.
    let mut soa = fill(SW * p, 0x1111_2222_3333_4444);
    let mut sstack = vec![0u32; SW * p]; // frame i at [i*SW .. i*SW+SW]
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for base in (0..p).step_by(W) {
            let m = Simd::<u32, W>::from_array(core::array::from_fn(|j| act[base + j] as u32))
                .simd_ne(V::splat(0));
            // save: scatter each word to lane-private frame.
            for w in 0..SW {
                let vals = V::from_slice(&soa[w * p + base..w * p + base + W]);
                let idx = Vi::from_array(core::array::from_fn(|j| (base + j) * SW + w));
                vals.scatter_select(&mut sstack, m.into(), idx);
            }
            // child mutates word 0.
            {
                let cur = V::from_slice(&soa[base..base + W]);
                m.select(cur + V::splat(1), cur).copy_to_slice(&mut soa[base..base + W]);
            }
            // restore: gather each word back.
            for w in 0..SW {
                let idx = Vi::from_array(core::array::from_fn(|j| (base + j) * SW + w));
                let got = V::gather_select(&sstack, m.into(), idx, V::splat(0));
                let cur = V::from_slice(&soa[w * p + base..w * p + base + W]);
                m.select(got, cur).copy_to_slice(&mut soa[w * p + base..w * p + base + W]);
            }
            chk = chk.wrapping_add(soa[base] as u64);
        }
    }
    let ns_scatter = t.elapsed().as_secs_f64() * 1e9 / branches;
    println!("  {:<14} {:>9.3} ns/branch   (chk {:#018x})", "simt-scatter", ns_scatter, chk);

    // simt-whole: snapshot the full warp frame with contiguous vector ld/st.
    let mut soa = fill(SW * p, 0x1111_2222_3333_4444);
    let mut wstack = vec![0u32; SW * W * (p / W)];
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for (g, base) in (0..p).step_by(W).enumerate() {
            let m = Simd::<u32, W>::from_array(core::array::from_fn(|j| act[base + j] as u32))
                .simd_ne(V::splat(0));
            for w in 0..SW {
                let vals = V::from_slice(&soa[w * p + base..w * p + base + W]);
                vals.copy_to_slice(&mut wstack[(g * SW + w) * W..(g * SW + w) * W + W]);
            }
            {
                let cur = V::from_slice(&soa[base..base + W]);
                m.select(cur + V::splat(1), cur).copy_to_slice(&mut soa[base..base + W]);
            }
            for w in 0..SW {
                let got = V::from_slice(&wstack[(g * SW + w) * W..(g * SW + w) * W + W]);
                let cur = V::from_slice(&soa[w * p + base..w * p + base + W]);
                m.select(got, cur).copy_to_slice(&mut soa[w * p + base..w * p + base + W]);
            }
            chk = chk.wrapping_add(soa[base] as u64);
        }
    }
    let ns_whole = t.elapsed().as_secs_f64() * 1e9 / branches;
    println!("  {:<14} {:>9.3} ns/branch   (chk {:#018x})", "simt-whole", ns_whole, chk);

    println!("\n  packing speedup (scalar / variant):  simt-scatter {:.2}x   simt-whole {:.2}x",
        ns_scalar / ns_scatter, ns_scalar / ns_whole);
    println!("  [>1 means the clone packs; scatter = realistic refill, whole = lockstep bound]");
}
