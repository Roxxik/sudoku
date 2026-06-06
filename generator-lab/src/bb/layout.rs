//! Banding layout: the cell ↔ (lane, bit) maps for the two transposed bandings,
//! every precomputed lookup table the closures read (`SINGLE9`, `DROP_TRIP`,
//! `RM_LC_TRIP`/`CM_LC_TRIP`, …), and the small `Simd<u32, 4>` (`B`) helpers
//! (`nonzero`, `exactly_one`, `first_rm`, `triplet_occ`, …). Pure data and
//! arithmetic, shared by the baseline engine ([`super`]) and the SIMT prober
//! (which imports the cell-map fns). See the [`super`] module docs for *why* two
//! views exist.

use crate::grid::{BOX_UNITS, CELLS, COL_UNITS, PEERS, ROW_UNITS};
use std::simd::Simd;
use std::simd::cmp::SimdPartialEq;

/// One v128 holds a digit's three 27-bit bands in lanes 0/1/2; lane 3 stays zero.
pub(crate) type B = Simd<u32, 4>;
pub(crate) const ZERO: B = Simd::from_array([0, 0, 0, 0]);

// --- layout maps: cell <-> (lane, bit) for each banding -----------------------
pub(crate) const fn rm_lane(cell: usize) -> usize {
    (cell / 9) / 3
}
pub(crate) const fn rm_bit(cell: usize) -> usize {
    ((cell / 9) % 3) * 9 + cell % 9
}
pub(crate) const fn cm_lane(cell: usize) -> usize {
    (cell % 9) / 3
}
pub(crate) const fn cm_bit(cell: usize) -> usize {
    ((cell % 9) % 3) * 9 + cell / 9
}
/// Inverse of (`rm_lane`, `rm_bit`): the cell at row-major position (lane, bit).
#[inline(always)]
pub(crate) fn rm_cell(lane: usize, bit: u32) -> usize {
    let b = bit as usize;
    (3 * lane + b / 9) * 9 + b % 9
}
/// Inverse of (`cm_lane`, `cm_bit`): the cell at column-major position (lane, bit).
#[inline(always)]
pub(crate) fn cm_cell(lane: usize, bit: u32) -> usize {
    let b = bit as usize;
    (b % 9) * 9 + (3 * lane + b / 9)
}

pub(crate) const BITS_R: [[u32; 4]; CELLS] = build_bits(true);
const BITS_C: [[u32; 4]; CELLS] = build_bits(false);
pub(crate) const PEER_MASK_R: [[u32; 4]; CELLS] = build_peer_mask(true);
const PEER_MASK_C: [[u32; 4]; CELLS] = build_peer_mask(false);
/// 27 units (9 rows, 9 cols, 9 boxes) as cell lists, in scan order.
pub(crate) const UNIT_CELLS: [[usize; 9]; 27] = build_unit_cells();
/// The 27 units as row-major masks. Rows 0..9, cols 9..18, boxes 18..27. (Only
/// the row-major view needs unit masks: the baseline's fused closure does column
/// work on the column-major bands' in-lane tables, and its discrete ladder scans
/// every unit in the row-major view — columns cross-lane, which
/// `exactly_one`/`nonzero` handle.)
const UNIT_MASK_R: [[u32; 4]; 27] = build_unit_masks(true);

const fn build_bits(row_major: bool) -> [[u32; 4]; CELLS] {
    let mut b = [[0u32; 4]; CELLS];
    let mut c = 0;
    while c < CELLS {
        let (lane, bit) = if row_major { (rm_lane(c), rm_bit(c)) } else { (cm_lane(c), cm_bit(c)) };
        b[c][lane] = 1u32 << bit;
        c += 1;
    }
    b
}

const fn build_peer_mask(row_major: bool) -> [[u32; 4]; CELLS] {
    let mut m = [[0u32; 4]; CELLS];
    let mut i = 0;
    while i < CELLS {
        let mut k = 0;
        while k < 20 {
            let p = PEERS[i][k];
            let (lane, bit) = if row_major { (rm_lane(p), rm_bit(p)) } else { (cm_lane(p), cm_bit(p)) };
            m[i][lane] |= 1u32 << bit;
            k += 1;
        }
        i += 1;
    }
    m
}

const fn build_unit_cells() -> [[usize; 9]; 27] {
    let mut u = [[0usize; 9]; 27];
    let mut i = 0;
    while i < 9 {
        u[i] = ROW_UNITS[i];
        u[9 + i] = COL_UNITS[i];
        u[18 + i] = BOX_UNITS[i];
        i += 1;
    }
    u
}

const fn build_unit_masks(row_major: bool) -> [[u32; 4]; 27] {
    let mut m = [[0u32; 4]; 27];
    let mut u = 0;
    while u < 27 {
        let mut k = 0;
        while k < 9 {
            let c = UNIT_CELLS[u][k];
            let (lane, bit) = if row_major { (rm_lane(c), rm_bit(c)) } else { (cm_lane(c), cm_bit(c)) };
            m[u][lane] |= 1u32 << bit;
            k += 1;
        }
        u += 1;
    }
    m
}

/// Within-band locked-candidates self-elimination, fully precomputed.
///
/// A band is 9 **triplets** (3 band-rows × 3 box-columns, each a 3-cell minirow).
/// Within one band, *all* a digit's locked-candidate eliminations are decided by
/// the 9-bit "triplet occupancy" and only ever clear whole triplets:
/// - **pointing**: a box-column confined to one band-row clears that row's other
///   two triplets,
/// - **claiming**: a band-row confined to one box-column clears that box's other
///   two triplets.
///
/// `BAND_KEEP_OCC[occ]` is the locked-candidate fixpoint occupancy (the 9-bit set
/// of *surviving* triplets) for every occupancy. The same table serves both
/// views: in row-major bands it is box↔row LC, in col-major bands box↔column LC.
/// Occupancy bit `3*r + k` is the triplet at band-row `r`, box-column `k`.
const BAND_KEEP_OCC: [u32; 512] = build_band_keep();

const fn build_band_keep() -> [u32; 512] {
    let mut t = [0u32; 512];
    let mut occ = 0usize;
    while occ < 512 {
        let mut keep = occ as u32; // 9-bit triplet occupancy
        loop {
            let mut next = keep;
            // pointing: box-column k whose occupied triplets are a single band-row.
            let mut k = 0;
            while k < 3 {
                let r0 = (next >> k) & 1;
                let r1 = (next >> (3 + k)) & 1;
                let r2 = (next >> (6 + k)) & 1;
                if r0 + r1 + r2 == 1 {
                    let r = if r0 == 1 { 0 } else if r1 == 1 { 1 } else { 2 };
                    let mut kk = 0;
                    while kk < 3 {
                        if kk != k {
                            next &= !(1 << (r * 3 + kk));
                        }
                        kk += 1;
                    }
                }
                k += 1;
            }
            // claiming: band-row r whose occupied triplets are a single box-column.
            let mut r = 0;
            while r < 3 {
                let c0 = (next >> (r * 3)) & 1;
                let c1 = (next >> (r * 3 + 1)) & 1;
                let c2 = (next >> (r * 3 + 2)) & 1;
                if c0 + c1 + c2 == 1 {
                    let k = if c0 == 1 { 0 } else if c1 == 1 { 1 } else { 2 };
                    let mut rr = 0;
                    while rr < 3 {
                        if rr != r {
                            next &= !(1 << (rr * 3 + k));
                        }
                        rr += 1;
                    }
                }
                r += 1;
            }
            if next == keep {
                break;
            }
            keep = next;
        }
        t[occ] = keep;
        occ += 1;
    }
    t
}

/// The triplets a band drops under locked-candidates, keyed on occupancy:
/// `DROP_TRIP[occ] = occ & !BAND_KEEP_OCC[occ]` precomputed, so the per-scan LC
/// check is one table load instead of a load + `not` + `and` (the `not` was the
/// single hottest instruction in `band_update`). Nonzero only for the rare
/// occupancies that actually have an LC elimination.
pub(crate) const DROP_TRIP: [u32; 512] = build_drop_trip();

const fn build_drop_trip() -> [u32; 512] {
    let mut t = [0u32; 512];
    let mut occ = 0usize;
    while occ < 512 {
        t[occ] = occ as u32 & !BAND_KEEP_OCC[occ];
        occ += 1;
    }
    t
}

/// Hidden-single lookup for a 9-cell unit reduced to a 9-bit candidate mask: the
/// lone bit's index if exactly one is set, else `0xFF`. A row, box, or column
/// collapses to nine bits, so detecting (and locating) a hidden single is one
/// table load off the band value — the same band value the occupancy/LC read —
/// instead of a per-unit `exactly_one` + `trailing` scan.
pub(crate) const SINGLE9: [u8; 512] = build_single9();

const fn build_single9() -> [u8; 512] {
    let mut t = [0xFFu8; 512];
    let mut v = 1usize;
    while v < 512 {
        if v & (v - 1) == 0 {
            t[v] = v.trailing_zeros() as u8;
        }
        v += 1;
    }
    t
}

/// 9-bit row -> 3-bit triplet occupancy: bit `k` set iff the row's `k`-th
/// 3-cell triplet (bits `3k..3k+2`) holds any candidate. Lets [`triplet_occ`]
/// gather a band's 9-bit occupancy with three table loads instead of a long
/// shift chain.
const OCC3: [u8; 512] = build_occ3();

const fn build_occ3() -> [u8; 512] {
    let mut t = [0u8; 512];
    let mut v = 0usize;
    while v < 512 {
        let mut o = 0u8;
        if v & 0b000_000_111 != 0 {
            o |= 1;
        }
        if v & 0b000_111_000 != 0 {
            o |= 2;
        }
        if v & 0b111_000_000 != 0 {
            o |= 4;
        }
        t[v] = o;
        v += 1;
    }
    t
}

/// For a triplet dropped by row-major LC — band `b`, triplet `t = 3*r + k` — the
/// cell masks to clear in BOTH views. Row-major: lane `b`, the 3 bits at
/// `9*r + 3*k`. Col-major: the same three cells form a vertical-triplet — lane
/// `k`, bits `{R, 9+R, 18+R}` with `R = 3*b + r`.
pub(crate) const RM_LC_TRIP: [[([u32; 4], [u32; 4]); 9]; 3] = build_lc_trip(true);
/// The col-major analogue: band `b` (column-stack), triplet `t = 3*cc + br`.
/// Col-major: lane `b`, bits at `9*cc + 3*br`. Row-major: lane `br`, bits
/// `{C, 9+C, 18+C}` with `C = 3*b + cc`.
pub(crate) const CM_LC_TRIP: [[([u32; 4], [u32; 4]); 9]; 3] = build_lc_trip(false);

const fn build_lc_trip(row_major: bool) -> [[([u32; 4], [u32; 4]); 9]; 3] {
    let mut out = [[([0u32; 4], [0u32; 4]); 9]; 3];
    let mut b = 0;
    while b < 3 {
        let mut t = 0;
        while t < 9 {
            let a = t / 3; // band-row (rm) or col-within-stack (cm)
            let g = t % 3; // box-col (rm) or box-row (cm)
            let mut rmask = [0u32; 4];
            let mut cmask = [0u32; 4];
            if row_major {
                // row-major: cells global row R = 3b+a, cols 3g..3g+2.
                rmask[b] = 0b111 << (a * 9 + 3 * g);
                let big_r = 3 * b + a;
                cmask[g] = (1 << big_r) | (1 << (9 + big_r)) | (1 << (18 + big_r));
            } else {
                // col-major: cells global col C = 3b+a, rows 3g..3g+2.
                cmask[b] = 0b111 << (a * 9 + 3 * g);
                let big_c = 3 * b + a;
                rmask[g] = (1 << big_c) | (1 << (9 + big_c)) | (1 << (18 + big_c));
            }
            out[b][t] = (rmask, cmask);
            t += 1;
        }
        b += 1;
    }
    out
}

#[inline(always)]
pub(crate) fn bit_r(c: usize) -> B {
    Simd::from_array(BITS_R[c])
}
#[inline(always)]
pub(crate) fn bit_c(c: usize) -> B {
    Simd::from_array(BITS_C[c])
}
#[inline(always)]
pub(crate) fn peer_mask_r(c: usize) -> B {
    Simd::from_array(PEER_MASK_R[c])
}
#[inline(always)]
pub(crate) fn peer_mask_c(c: usize) -> B {
    Simd::from_array(PEER_MASK_C[c])
}
#[inline(always)]
pub(crate) fn unit_mask_r(u: usize) -> B {
    Simd::from_array(UNIT_MASK_R[u])
}
#[inline(always)]
pub(crate) fn nonzero(x: B) -> bool {
    x.simd_ne(ZERO).any()
}
/// Total set bits across the three bands. `u32::count_ones` is one wasm/ARM
/// instruction, so this stays cheap where an actual count is needed.
#[inline(always)]
pub(crate) fn popcnt(x: B) -> u32 {
    x[0].count_ones() + x[1].count_ones() + x[2].count_ones()
}
/// Exactly one bit set across the whole banded value — popcount-free. Exactly one
/// band is nonzero and that band holds a single power of two.
#[inline(always)]
pub(crate) fn exactly_one(x: B) -> bool {
    let (a, b, c) = (x[0], x[1], x[2]);
    let nz = (a != 0) as u32 + (b != 0) as u32 + (c != 0) as u32;
    nz == 1 && {
        let v = a | b | c;
        v & (v - 1) == 0
    }
}
/// At least two bits set across the bands — popcount-free (the `< 2` guard the
/// locked-candidate scans want, phrased without a count).
#[inline(always)]
pub(crate) fn at_least_two(x: B) -> bool {
    nonzero(x) && !exactly_one(x)
}
/// Cell of the lowest set bit in a row-major value.
#[inline(always)]
pub(crate) fn first_rm(x: B) -> usize {
    if x[0] != 0 {
        rm_cell(0, x[0].trailing_zeros())
    } else if x[1] != 0 {
        rm_cell(1, x[1].trailing_zeros())
    } else {
        rm_cell(2, x[2].trailing_zeros())
    }
}
/// 9-bit triplet occupancy of one band's 27-bit candidate set: bit `3*r + k` set
/// iff triplet (band-row `r`, box-column `k`) holds any candidate. Three `OCC3`
/// loads off the band's three 9-bit row chunks, gathered into the 9-bit result.
#[inline(always)]
pub(crate) fn triplet_occ(m: u32) -> usize {
    OCC3[(m & 0x1FF) as usize] as usize
        | (OCC3[((m >> 9) & 0x1FF) as usize] as usize) << 3
        | (OCC3[((m >> 18) & 0x1FF) as usize] as usize) << 6
}
