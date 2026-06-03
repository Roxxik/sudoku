//! Validates the `lanes/bands` packing factor for the DOMINANT cost — the
//! `band_update` hidden-single sweep — by actually running it three ways and
//! timing them on this machine:
//!
//!   scalar-per-band : the shipped style. Per (digit, band) it extracts ONE 27-bit
//!                     band as a scalar u32 and does table lookups (SINGLE9) per
//!                     row/box. The 3-band v128 packing does NOT help this sweep —
//!                     it's scalar u32 work, so it's the real baseline to beat.
//!   simt-gather     : SoA across W=8 puzzles (Simd<u32,8>), but hidden-single
//!                     detection via a vector GATHER into SINGLE9 — the naive port,
//!                     to expose the gather tax (vpgatherdd is not 1/clock).
//!   simt-alu        : SoA across W=8 puzzles, detection by pure vector ALU
//!                     (exactly-one = v!=0 & v&(v-1)==0). NO gather, NO table. This
//!                     is the gather-free kernel the no-LC design enables (dropping
//!                     LC also drops the only forced gather, DROP_TRIP; inflation
//!                     measured at 1.06x). The ratio scalar/simt-alu is the realized
//!                     packing speedup for the sweep.
//!
//! All three fold an identical decision (per row/box: is there a lone candidate,
//! and a checksum of the band) so none is optimized away and they do equal work.
//! The sweep is control-flow uniform, so random bands at realistic density are a
//! faithful throughput proxy.
//!
//! Usage: cargo run --release -p generator-lab --example sweepbench -- [--puzzles P=8192] [--iters I=400]

#![feature(portable_simd)]

use std::simd::cmp::SimdPartialEq;
use std::simd::{Select, Simd, num::SimdUint};
use std::time::Instant;

const W: usize = 8;
type V = Simd<u32, W>;

/// SINGLE9[v] = index of the lone set bit of a 9-bit `v`, or 0xFF. Same table as
/// bb.rs, materialized locally so the gather variant can index it.
fn single9() -> [u32; 512] {
    let mut t = [0xFFu32; 512];
    for v in 1usize..512 {
        if v & (v - 1) == 0 {
            t[v] = v.trailing_zeros();
        }
    }
    t
}

/// One puzzle's row-major candidate state: 9 digit-bands + the unsolved-band, each
/// a 27-bit value in lanes 0..3 of nothing — here just 3 scalar u32 bands.
#[derive(Clone, Copy)]
struct Bands {
    d: [[u32; 3]; 9], // d[digit][band]
    u: [u32; 3],      // unsolved per band
}

fn gen_bands(n: usize) -> Vec<Bands> {
    let mut s: u64 = 0x1234_5678_9abc_def1;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    // realistic-ish 27-bit band density: AND two draws so cells aren't all live.
    let mut m27 = move || (next() as u32 & 0x07ff_ffff) & (next() as u32 | 0x00ff_00ff);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let u = [m27(), m27(), m27()];
        let d = core::array::from_fn(|_| [m27() & u[0], m27() & u[1], m27() & u[2]]);
        out.push(Bands { d, u });
    }
    out
}

#[inline(always)]
fn scalar_sweep(p: &Bands) -> u64 {
    let t = SINGLE9;
    let mut acc = 0u64;
    for d in 0..9 {
        for b in 0..3 {
            let live = p.d[d][b] & p.u[b];
            // three rows
            for rr in 0..3 {
                let chunk = (live >> (9 * rr)) & 0x1ff;
                let s = t[chunk as usize];
                acc = acc.wrapping_add(chunk as u64).wrapping_add((s != 0xFF) as u64);
            }
            // three boxes (gather the 9 bits of each box)
            for k in 0..3 {
                let bk = ((live >> (3 * k)) & 7)
                    | (((live >> (9 + 3 * k)) & 7) << 3)
                    | (((live >> (18 + 3 * k)) & 7) << 6);
                let s = t[bk as usize];
                acc = acc.wrapping_add(bk as u64).wrapping_add((s != 0xFF) as u64);
            }
        }
    }
    acc
}

/// Load lane j of `grp[j]` for band `b`, digit `d`, AND unsolved.
#[inline(always)]
fn live_v(grp: &[Bands], off: usize, d: usize, b: usize) -> V {
    let live: [u32; W] = core::array::from_fn(|j| grp[off + j].d[d][b] & grp[off + j].u[b]);
    V::from_array(live)
}

#[inline(always)]
fn simt_alu(grp: &[Bands], off: usize) -> V {
    let one = V::splat(1);
    let mut acc = V::splat(0);
    for d in 0..9 {
        for b in 0..3 {
            let live = live_v(grp, off, d, b);
            for rr in 0..3u32 {
                let chunk = (live >> V::splat(9 * rr)) & V::splat(0x1ff);
                let nz = chunk.simd_ne(V::splat(0));
                let pow2 = (chunk & (chunk - one)).simd_eq(V::splat(0));
                let single = (nz & pow2).select(one, V::splat(0));
                acc += chunk + single;
            }
            for k in 0..3u32 {
                let bk = ((live >> V::splat(3 * k)) & V::splat(7))
                    | (((live >> V::splat(9 + 3 * k)) & V::splat(7)) << V::splat(3))
                    | (((live >> V::splat(18 + 3 * k)) & V::splat(7)) << V::splat(6));
                let nz = bk.simd_ne(V::splat(0));
                let pow2 = (bk & (bk - one)).simd_eq(V::splat(0));
                let single = (nz & pow2).select(one, V::splat(0));
                acc += bk + single;
            }
        }
    }
    acc
}

#[inline(always)]
fn simt_gather(grp: &[Bands], off: usize) -> V {
    let t = SINGLE9;
    let mut acc = V::splat(0);
    for d in 0..9 {
        for b in 0..3 {
            let live = live_v(grp, off, d, b);
            for rr in 0..3u32 {
                let chunk = (live >> V::splat(9 * rr)) & V::splat(0x1ff);
                let idx: Simd<usize, W> = chunk.cast();
                let s = Simd::<u32, W>::gather_or_default(&t, idx);
                let hit = s.simd_ne(V::splat(0xFF)).select(V::splat(1), V::splat(0));
                acc += chunk + hit;
            }
            for k in 0..3u32 {
                let bk = ((live >> V::splat(3 * k)) & V::splat(7))
                    | (((live >> V::splat(9 + 3 * k)) & V::splat(7)) << V::splat(3))
                    | (((live >> V::splat(18 + 3 * k)) & V::splat(7)) << V::splat(6));
                let idx: Simd<usize, W> = bk.cast();
                let s = Simd::<u32, W>::gather_or_default(&t, idx);
                let hit = s.simd_ne(V::splat(0xFF)).select(V::splat(1), V::splat(0));
                acc += bk + hit;
            }
        }
    }
    acc
}

static SINGLE9: [u32; 512] = {
    // const-fn fill (mirror of single9()).
    let mut t = [0xFFu32; 512];
    let mut v = 1usize;
    while v < 512 {
        if v & (v - 1) == 0 {
            t[v] = v.trailing_zeros();
        }
        v += 1;
    }
    t
};

fn main() {
    let _ = single9();
    let mut puzzles = 8192usize;
    let mut iters = 400usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--puzzles" => puzzles = it.next().and_then(|s| s.parse().ok()).unwrap_or(puzzles),
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            _ => {}
        }
    }
    puzzles -= puzzles % W;
    let data = gen_bands(puzzles);
    let sweeps = (puzzles * iters) as f64;

    // warm + time each variant.
    let time = |name: &str, f: &dyn Fn() -> u64| -> f64 {
        let mut acc = 0u64;
        let t = Instant::now();
        for _ in 0..iters {
            acc = acc.wrapping_add(f());
        }
        let s = t.elapsed().as_secs_f64();
        let ns = s * 1e9 / sweeps;
        println!("  {:<16} {:>8.3} ns/puzzle-sweep   (chk {:#018x})", name, ns, acc);
        ns
    };

    println!("sweepbench: {puzzles} puzzles x {iters} iters, W={W} (band_update hidden-single sweep)\n");
    let ns_scalar = time("scalar-per-band", &|| data.iter().map(scalar_sweep).fold(0u64, u64::wrapping_add));
    let ns_gather = time("simt-gather", &|| {
        (0..data.len()).step_by(W).map(|o| simt_gather(&data, o).reduce_sum() as u64).fold(0, u64::wrapping_add)
    });
    let ns_alu = time("simt-alu", &|| {
        (0..data.len()).step_by(W).map(|o| simt_alu(&data, o).reduce_sum() as u64).fold(0, u64::wrapping_add)
    });

    println!("\n  packing speedup (scalar / variant):  simt-alu {:.2}x   simt-gather {:.2}x", ns_scalar / ns_alu, ns_scalar / ns_gather);
    println!("  gather tax (alu / gather):           {:.2}x slower", ns_gather / ns_alu);
    println!("  [W=8; >8/3=2.67 means the sweep packs better than the 3-band baseline]");
}
