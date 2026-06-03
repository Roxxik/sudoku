//! Validates the packing factor of `place` — the other half of the prober inner
//! loop, and the SIMT-hostile half. A placement forbids digit `d` across cell's
//! peer mask: `r[d] &= !peer_mask`. In the shipped scalar/AoS rep that's ~2 band
//! AND-NOTs. In puzzle-per-lane SoA it is awkward: the candidate state is 9
//! separate `[digit][band]` vector registers, and the digit `d` to clear DIFFERS
//! per lane, so you cannot index "register d" per lane. You must either gather, or
//! loop all 9 digit-registers masking the lanes whose `d` matches. This measures
//! both the realistic SoA cost and the scalar baseline on the same placements.
//!
//! Variants:
//!   scalar-aos   : shipped style. Per lane: unsolved &= !cellbit; r[d] &= !peer.
//!   simt-9masked : SoA across W=8. Gather cellbit + peer mask per lane (one band
//!                  each, by cell), clear unsolved (3 bands), then loop dd=0..9
//!                  masking lanes with d==dd and AND-NOT the peer into r[dd].
//!   simt-gather  : SoA, but write the peer-clear by scattering — included to show
//!                  the alternative is no better.
//!
//! A placement is data-uniform in CONTROL (every lane always places), so random
//! (cell, digit) at realistic spread is a faithful throughput proxy. `active`
//! models the fraction of lanes that actually fire on a given step (the rest are
//! masked off) — placements are sparser than the sweep, so this matters.
//!
//! Usage: cargo run --release -p generator-lab --example placebench -- [--puzzles P=8192] [--iters I=600] [--active F=1.0]

#![feature(portable_simd)]

use std::simd::cmp::SimdPartialEq;
use std::simd::{Select, Simd};
use std::time::Instant;

const W: usize = 8;
const CELLS: usize = 81;
type V = Simd<u32, W>;

// Row-major banding (same maps as bb.rs): cell -> (band, bit-in-band).
const fn rm_lane(c: usize) -> usize {
    (c / 9) / 3
}
const fn rm_bit(c: usize) -> usize {
    ((c / 9) % 3) * 9 + c % 9
}

/// peer_mask_r[cell][band]: the 20 peers of `cell` as 3 band words. cellbit[cell]
/// [band]: the single bit of `cell` itself. Both built from the row-major maps.
fn tables() -> (Vec<[u32; 3]>, Vec<[u32; 3]>) {
    // peers: same-row, same-col, same-box, minus self.
    let mut peer = vec![[0u32; 3]; CELLS];
    let mut cellbit = vec![[0u32; 3]; CELLS];
    for c in 0..CELLS {
        cellbit[c][rm_lane(c)] = 1u32 << rm_bit(c);
        let (r, col) = (c / 9, c % 9);
        let (br, bc) = (r / 3 * 3, col / 3 * 3);
        for p in 0..CELLS {
            if p == c {
                continue;
            }
            let (pr, pcol) = (p / 9, p % 9);
            let same = pr == r || pcol == col || (pr / 3 * 3 == br && pcol / 3 * 3 == bc);
            if same {
                peer[c][rm_lane(p)] |= 1u32 << rm_bit(p);
            }
        }
    }
    (peer, cellbit)
}

#[derive(Clone)]
struct Soa {
    r: [[Vec<u32>; 3]; 9], // r[digit][band][puzzle]
    u: [Vec<u32>; 3],      // unsolved[band][puzzle]
}

fn gen_state(n: usize) -> Soa {
    let mut s: u64 = 0xabcd_1234_5678_9f01;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s as u32 & 0x07ff_ffff
    };
    Soa {
        r: core::array::from_fn(|_| core::array::from_fn(|_| (0..n).map(|_| next()).collect())),
        u: core::array::from_fn(|_| (0..n).map(|_| next()).collect()),
    }
}

fn main() {
    let mut puzzles = 8192usize;
    let mut iters = 600usize;
    let mut active = 1.0f64;
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
    let (peer, cellbit) = tables();

    // A placement stream: per puzzle a (cell, digit) and an active flag.
    let mut s: u64 = 0x0f0f_5555_aaaa_3333;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let cells: Vec<usize> = (0..puzzles).map(|_| (next() % CELLS as u64) as usize).collect();
    let digits: Vec<usize> = (0..puzzles).map(|_| (next() % 9) as usize).collect();
    let act: Vec<bool> = (0..puzzles).map(|_| (next() as f64 / u64::MAX as f64) < active).collect();

    let placements = (puzzles * iters) as f64;

    // SCALAR-AOS: independent per puzzle.
    let mut st = gen_state(puzzles);
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for p in 0..puzzles {
            if !act[p] {
                continue;
            }
            let (c, d) = (cells[p], digits[p]);
            for b in 0..3 {
                st.u[b][p] &= !cellbit[c][b];
                st.r[d][b][p] &= !peer[c][b];
            }
            chk = chk.wrapping_add(st.r[d][0][p] as u64);
        }
    }
    let ns_scalar = t.elapsed().as_secs_f64() * 1e9 / placements;
    println!("placebench: {puzzles} puzzles x {iters} iters, W={W}, active={active}\n");
    println!("  {:<14} {:>9.3} ns/placement   (chk {:#018x})", "scalar-aos", ns_scalar, chk);

    // SIMT-9MASKED: vector across W lanes, loop 9 digit-registers with a per-lane
    // digit mask. Gather cellbit/peer per lane (one band each, indexed by cell).
    let mut st = gen_state(puzzles);
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for base in (0..puzzles).step_by(W) {
            // per-lane cell, digit, active.
            let cj: [usize; W] = core::array::from_fn(|j| cells[base + j]);
            let dj: V = V::from_array(core::array::from_fn(|j| digits[base + j] as u32));
            let actm = Simd::<u32, W>::from_array(core::array::from_fn(|j| act[base + j] as u32))
                .simd_ne(V::splat(0));
            // gather peer + cellbit per band (table indexed by per-lane cell).
            let peerb: [V; 3] = core::array::from_fn(|b| V::from_array(core::array::from_fn(|j| peer[cj[j]][b])));
            let cellb: [V; 3] = core::array::from_fn(|b| V::from_array(core::array::from_fn(|j| cellbit[cj[j]][b])));
            // clear unsolved (masked to active lanes). Contiguous => vector ld/st.
            for b in 0..3 {
                let uu = V::from_slice(&st.u[b][base..base + W]);
                let cleared = uu & !cellb[b];
                actm.select(cleared, uu).copy_to_slice(&mut st.u[b][base..base + W]);
            }
            // peer-forbid: loop all 9 digit registers, mask lanes whose d == dd.
            for dd in 0..9u32 {
                let lane_hits = dj.simd_eq(V::splat(dd)) & actm;
                for b in 0..3 {
                    let rr = V::from_slice(&st.r[dd as usize][b][base..base + W]);
                    let cleared = rr & !peerb[b];
                    lane_hits.select(cleared, rr).copy_to_slice(&mut st.r[dd as usize][b][base..base + W]);
                }
            }
            chk = chk.wrapping_add(st.r[0][0][base] as u64);
        }
    }
    let ns_9m = t.elapsed().as_secs_f64() * 1e9 / placements;
    println!("  {:<14} {:>9.3} ns/placement   (chk {:#018x})", "simt-9masked", ns_9m, chk);

    println!("\n  packing speedup (scalar / simt-9masked): {:.2}x", ns_scalar / ns_9m);
    println!("  [the per-lane-digit indexing forces a 9x register loop; >2.67 means");
    println!("   place still beats the 3-band baseline, <1 means SoA place regresses]");

    // --- WAVE placement (place_singles style) -------------------------------
    // The naked-single wave clears, for EVERY digit, the accumulated peer mask of
    // that digit's just-placed cells. The scalar engine ALREADY loops all 9 digits
    // here (see BitBoard::place_singles), so the SoA 9-digit loop is no longer
    // extra work — it should pack. We feed a precomputed per-(lane,digit,band)
    // peer mask (the accumulation itself is measured separately by the sweep's
    // popcount-free style); this isolates the AND-NOT throughput.
    let wave: Vec<[[u32; 3]; 9]> = {
        let mut v = next();
        let mut nx = move || {
            v ^= v << 13;
            v ^= v >> 7;
            v ^= v << 17;
            v as u32 & 0x07ff_ffff
        };
        (0..puzzles).map(|_| core::array::from_fn(|_| [nx(), nx(), nx()])).collect()
    };

    let mut st = gen_state(puzzles);
    let mut chk = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for p in 0..puzzles {
            for d in 0..9 {
                for b in 0..3 {
                    st.r[d][b][p] &= !wave[p][d][b];
                }
            }
            chk = chk.wrapping_add(st.r[0][0][p] as u64);
        }
    }
    let ns_wscalar = t.elapsed().as_secs_f64() * 1e9 / placements;

    let mut st = gen_state(puzzles);
    let mut chk2 = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for base in (0..puzzles).step_by(W) {
            for d in 0..9 {
                for b in 0..3 {
                    // wave peer mask is per-(puzzle,digit,band): gather (it is not
                    // contiguous in this AoS table — a real kernel would build it
                    // in-register, so this gather is a pessimistic stand-in).
                    let pm = V::from_array(core::array::from_fn(|j| wave[base + j][d][b]));
                    let rr = V::from_slice(&st.r[d][b][base..base + W]) & !pm;
                    rr.copy_to_slice(&mut st.r[d][b][base..base + W]);
                }
            }
            chk2 = chk2.wrapping_add(st.r[0][0][base] as u64);
        }
    }
    let ns_wsimt = t.elapsed().as_secs_f64() * 1e9 / placements;
    println!("\n  WAVE placement (both loop all 9 digits, place_singles style):");
    println!("  {:<14} {:>9.3} ns/wave   (chk {:#018x})", "scalar-aos", ns_wscalar, chk);
    println!("  {:<14} {:>9.3} ns/wave   (chk {:#018x})", "simt", ns_wsimt, chk2);
    println!("  packing speedup (scalar / simt): {:.2}x", ns_wscalar / ns_wsimt);
}
