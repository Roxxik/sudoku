//! Shared **dual-banded** bitboard core for BOTH the uniqueness prober
//! (existence DFS) and the baseline technique engine.
//!
//! ## Why two views
//!
//! A **band** is 3 lines = 27 cells = three boxes side by side, stored in a
//! `Simd<u32, 4>` (lanes 0/1/2 = the three bands, lane 3 unused). In a single
//! row-major banding, rows and boxes are *in-lane* (a unit is a constant `[u32;
//! 4]` mask, every test is plain `u32` work) but **columns straddle all three
//! bands** — the one awkward unit. So we keep the candidates in two transposed
//! copies:
//!
//! - `r[d]` — **row-major** bands (lane `= row/3`, bit `= (row%3)*9 + col`):
//!   rows and boxes are in-lane.
//! - `c[d]` — **column-major** bands (lane `= col/3`, bit `= (col%3)*9 + row`):
//!   columns and boxes are in-lane.
//!
//! Between them *every* unit is in-lane in at least one view, so all hidden
//! singles and all locked candidates are cheap and popcount-free, and the one
//! within-band locked-candidates table ([`BAND_KEEP_OCC`]) applies to both.
//!
//! ## Keeping the two views in sync is cheap
//!
//! - **Placements** (naked/hidden singles, branch decisions) sync for free: a
//!   placement clears a precomputed peer-mask, and we just clear it in *both*
//!   layouts (`peer_mask_r` and `peer_mask_c`).
//! - **Locked-candidates eliminations** are the only cross-view effect, and they
//!   transpose *cleanly*: locked candidates clear whole **triplets**, and a
//!   row-major triplet `(band, band-row, box-col)` is exactly a col-major
//!   vertical-triplet. So LC runs at triplet granularity and each dropped triplet
//!   clears a precomputed mask in *both* views — a 27-entry lookup, never a
//!   general 81-bit transpose.
//!
//! ## Faithfulness without step-matching
//!
//! The strip trajectory depends only on two facts from the baseline solve:
//! whether it `solved`, and whether each required technique fired at least once.
//! Both are order-independent (the deductive closure is unique), so these
//! techniques need only be *sound* and *complete*, NOT step-for-step identical to
//! the scalar twins. Everything the prober adds (both-view hidden singles, both
//! orientations of LC) is sound — it only sharpens the existence search, never
//! changes its yes/no verdict — so the generator fingerprint is invariant.
//! `tests/bb_equiv.rs` cross-checks the baseline against the scalar engine;
//! the generator's `find 118329` anchor pins it end-to-end.

use crate::grid::{BOX_UNITS, Board, COL_UNITS, CELLS, PEERS, ROW_UNITS, iter_digits};
use crate::techniques::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, LC_CLAIMING, LC_POINTING, Mask,
    NAKED_PAIR, NAKED_QUAD, NAKED_SINGLE, NAKED_TRIPLE, NUM, Outcome,
};
use crate::util::for_each_combination;
use std::simd::Simd;
use std::simd::cmp::SimdPartialEq;

/// One v128 holds a digit's three 27-bit bands in lanes 0/1/2; lane 3 stays zero.
type B = Simd<u32, 4>;
const ZERO: B = Simd::from_array([0, 0, 0, 0]);

// --- optional baseline anatomy counters (feature = "count") -------------------
// Count how often each technique is SCANNED per attempt (scan count × scan size
// ≈ cost), to see what dominates `baseline` before optimizing it.
pub const CTR_NAMES: [&str; 8] = [
    "baseline-calls", "sieve-waves", "hidden_single", "lc_pointing", "lc_claiming",
    "naked_subset", "hidden_subset", "cell_candidates",
];
#[cfg(feature = "count")]
static CTR: [core::sync::atomic::AtomicU64; 8] = [const { core::sync::atomic::AtomicU64::new(0) }; 8];
#[inline(always)]
fn bump(_i: usize) {
    #[cfg(feature = "count")]
    CTR[_i].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}
#[cfg(feature = "count")]
pub fn ctr_snapshot() -> [u64; 8] {
    core::array::from_fn(|i| CTR[i].load(core::sync::atomic::Ordering::Relaxed))
}
#[cfg(feature = "count")]
pub fn ctr_reset() {
    for a in CTR.iter() {
        a.store(0, core::sync::atomic::Ordering::Relaxed);
    }
}

// --- prober anatomy counters (feature = "count") ------------------------------
// Where does the existence-DFS cost go: propagation (singles waves) vs branching
// (recursion / clones)? Single-threaded lab, so plain mutable statics — see
// solver-lab/src/counters.rs.
pub const PCTR_NAMES: [&str; 8] = [
    "alt-calls", "alt-nonunique", "solve_first-nodes", "sieve-waves",
    "place_singles-calls", "branch-points", "child-clones", "branch-digits",
];
#[cfg(feature = "count")]
static mut PCTR: [u64; 8] = [0; 8];
// SAFETY: single-threaded use only; reads/writes a `u64` by value, no aliasing.
#[inline(always)]
fn pbump(_i: usize) {
    #[cfg(feature = "count")]
    unsafe {
        PCTR[_i] += 1;
    }
}
#[cfg(feature = "count")]
pub fn pctr_snapshot() -> [u64; 8] {
    unsafe { PCTR }
}
#[cfg(feature = "count")]
pub fn pctr_reset() {
    unsafe {
        PCTR = [0; 8];
    }
}

// --- band_update scan metrics (feature = "count") -----------------------------
// Sizes the dirty-band-tracking opportunity: 0 propagate-calls, 1 band-passes
// (one rm+cm fixpoint iteration), 2 bd-scans (per-(band,digit) iterations across
// rm+cm), 3 bd-productive (scans that actually dropped a triplet or placed a
// single). waste = 1 - productive/scans = the rescans dirty-tracking could skip.
pub const BAND_CTR_NAMES: [&str; 4] = ["propagate-calls", "band-passes", "bd-scans", "bd-productive"];
#[cfg(feature = "count")]
static mut BAND_CTR: [u64; 4] = [0; 4];
#[cfg(feature = "count")]
pub fn band_ctr_snapshot() -> [u64; 4] {
    unsafe { BAND_CTR }
}
#[cfg(feature = "count")]
pub fn band_ctr_reset() {
    unsafe {
        BAND_CTR = [0; 4];
    }
}

/// One existence-DFS query (`any_alt_solves`): the board sparsity it ran on, the
/// work it cost, and whether it found a 2nd solution. Lets a diagnostic see WHERE
/// prober time concentrates (sparse vs dense boards; unique-proving vs
/// alt-finding) instead of just totals.
#[cfg(feature = "count")]
#[derive(Clone, Copy)]
pub struct AltStat {
    pub empties: u16,
    pub nodes: u32,
    pub guesses: u32,
    pub nonunique: bool,
}
#[cfg(feature = "count")]
static mut ALT_STATS: Vec<AltStat> = Vec::new();
#[cfg(feature = "count")]
pub fn alt_stats() -> &'static [AltStat] {
    unsafe { &*core::ptr::addr_of!(ALT_STATS) }
}
#[cfg(feature = "count")]
pub fn alt_stats_reset() {
    unsafe {
        (*core::ptr::addr_of_mut!(ALT_STATS)).clear();
    }
}

// --- layout maps: cell <-> (lane, bit) for each banding -----------------------
const fn rm_lane(cell: usize) -> usize {
    (cell / 9) / 3
}
const fn rm_bit(cell: usize) -> usize {
    ((cell / 9) % 3) * 9 + cell % 9
}
const fn cm_lane(cell: usize) -> usize {
    (cell % 9) / 3
}
const fn cm_bit(cell: usize) -> usize {
    ((cell % 9) % 3) * 9 + cell / 9
}
/// Inverse of (`rm_lane`, `rm_bit`): the cell at row-major position (lane, bit).
#[inline(always)]
fn rm_cell(lane: usize, bit: u32) -> usize {
    let b = bit as usize;
    (3 * lane + b / 9) * 9 + b % 9
}
/// Inverse of (`cm_lane`, `cm_bit`): the cell at column-major position (lane, bit).
#[inline(always)]
fn cm_cell(lane: usize, bit: u32) -> usize {
    let b = bit as usize;
    (b % 9) * 9 + (3 * lane + b / 9)
}

const BITS_R: [[u32; 4]; CELLS] = build_bits(true);
const BITS_C: [[u32; 4]; CELLS] = build_bits(false);
const PEER_MASK_R: [[u32; 4]; CELLS] = build_peer_mask(true);
const PEER_MASK_C: [[u32; 4]; CELLS] = build_peer_mask(false);
/// 27 units (9 rows, 9 cols, 9 boxes) as cell lists, in scan order.
const UNIT_CELLS: [[usize; 9]; 27] = build_unit_cells();
/// The 27 units as row-major masks. Rows 0..9, cols 9..18, boxes 18..27. (Only
/// the row-major view needs unit masks: the prober's column work rides the
/// column-major bands' in-lane tables, and the baseline scans every unit in the
/// row-major view — columns cross-lane, which `exactly_one`/`nonzero` handle.)
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

/// Hidden-single lookup for a 9-cell unit reduced to a 9-bit candidate mask: the
/// lone bit's index if exactly one is set, else `0xFF`. A row, box, or column
/// collapses to nine bits, so detecting (and locating) a hidden single is one
/// table load off the band value — the same band value the occupancy/LC read —
/// instead of a per-unit `exactly_one` + `trailing` scan.
const SINGLE9: [u8; 512] = build_single9();

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
const RM_LC_TRIP: [[([u32; 4], [u32; 4]); 9]; 3] = build_lc_trip(true);
/// The col-major analogue: band `b` (column-stack), triplet `t = 3*cc + br`.
/// Col-major: lane `b`, bits at `9*cc + 3*br`. Row-major: lane `br`, bits
/// `{C, 9+C, 18+C}` with `C = 3*b + cc`.
const CM_LC_TRIP: [[([u32; 4], [u32; 4]); 9]; 3] = build_lc_trip(false);

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
fn bit_r(c: usize) -> B {
    Simd::from_array(BITS_R[c])
}
#[inline(always)]
fn bit_c(c: usize) -> B {
    Simd::from_array(BITS_C[c])
}
#[inline(always)]
fn peer_mask_r(c: usize) -> B {
    Simd::from_array(PEER_MASK_R[c])
}
#[inline(always)]
fn peer_mask_c(c: usize) -> B {
    Simd::from_array(PEER_MASK_C[c])
}
#[inline(always)]
fn unit_mask_r(u: usize) -> B {
    Simd::from_array(UNIT_MASK_R[u])
}
#[inline(always)]
fn nonzero(x: B) -> bool {
    x.simd_ne(ZERO).any()
}
/// Total set bits across the three bands. `u32::count_ones` is one wasm/ARM
/// instruction, so this stays cheap where an actual count is needed.
#[inline(always)]
fn popcnt(x: B) -> u32 {
    x[0].count_ones() + x[1].count_ones() + x[2].count_ones()
}
/// Exactly one bit set across the whole banded value — popcount-free. Exactly one
/// band is nonzero and that band holds a single power of two.
#[inline(always)]
fn exactly_one(x: B) -> bool {
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
fn at_least_two(x: B) -> bool {
    nonzero(x) && !exactly_one(x)
}
/// Cell of the lowest set bit in a row-major value.
#[inline(always)]
fn first_rm(x: B) -> usize {
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
fn triplet_occ(m: u32) -> usize {
    OCC3[(m & 0x1FF) as usize] as usize
        | (OCC3[((m >> 9) & 0x1FF) as usize] as usize) << 3
        | (OCC3[((m >> 18) & 0x1FF) as usize] as usize) << 6
}

/// The nine digit boards in both bandings, plus the empty-cell mask in both.
/// `r`/`unsolved_r` are row-major (rows & boxes in-lane); `c`/`unsolved_c` are
/// column-major (columns & boxes in-lane). The two are kept consistent at every
/// mutation.
#[derive(Clone, PartialEq)]
pub struct BitBoard {
    r: [B; 9],
    c: [B; 9],
    unsolved_r: B,
    unsolved_c: B,
}

struct Sieve {
    ones: B,
    twos: B,
}

/// Per-digit *clue* positions in the row-major banding: bit for cell `i` of
/// `r[d-1]` is set iff cell `i` holds the clue digit `d`. This is the one fact
/// the candidate bands can't cheaply answer — which surviving peer holds which
/// digit — so `apply_clear` reads it to recompute reopened candidates with band
/// ops instead of a per-peer scan of the scalar grid. Kept *outside* `BitBoard`:
/// the solver never touches it, so it must not bloat the per-branch clone in
/// `solve_first`. Only the row-major banding is stored — `apply_clear` enumerates
/// it to get clue *cells*, then indexes both peer-mask tables by cell.
pub struct Placed {
    r: [B; 9],
}

impl Placed {
    pub fn from_board(b: &Board) -> Self {
        let mut r = [[0u32; 4]; 9];
        for cell in 0..CELLS {
            let d = b.cell(cell);
            if d != 0 {
                r[(d - 1) as usize][rm_lane(cell)] |= 1u32 << rm_bit(cell);
            }
        }
        Placed { r: r.map(Simd::from_array) }
    }
}

/// Why band propagation stopped.
enum Prop {
    /// Every cell is filled — a completion exists on this line.
    Solved,
    /// A cell ran out of candidates — this line is dead.
    Contradiction,
    /// A fixpoint with empty cells remaining — the caller must branch.
    Stuck,
}

impl BitBoard {
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn from_board(b: &Board) -> Self {
        let mut r = [[0u32; 4]; 9];
        let mut c = [[0u32; 4]; 9];
        let mut ur = [0u32; 4];
        let mut uc = [0u32; 4];
        for cell in 0..CELLS {
            if b.cell(cell) == 0 {
                ur[rm_lane(cell)] |= 1u32 << rm_bit(cell);
                uc[cm_lane(cell)] |= 1u32 << cm_bit(cell);
                for d in iter_digits(b.candidates(cell)) {
                    r[(d - 1) as usize][rm_lane(cell)] |= 1u32 << rm_bit(cell);
                    c[(d - 1) as usize][cm_lane(cell)] |= 1u32 << cm_bit(cell);
                }
            }
        }
        BitBoard {
            r: r.map(Simd::from_array),
            c: c.map(Simd::from_array),
            unsolved_r: Simd::from_array(ur),
            unsolved_c: Simd::from_array(uc),
        }
    }

    /// Re-open cell `i` (which held digit `d0`) in both views, keeping
    /// `self == from_board(board_from_cells(cells))` without a scalar *candidate*
    /// shadow — bb owns all candidate state. `placed` is the per-digit clue map
    /// (the one thing bb can't derive: which digit a surviving peer holds); this
    /// method drops cell `i` from it first, then re-opens candidates in two
    /// places: the **cleared cell** (its column becomes its naked candidates —
    /// every digit no still-present peer holds) and **its peers** (`d0` returns to
    /// the empty peers no *other* present peer still blocks). Both are pure band
    /// ops off `placed` — no per-peer scan of the scalar grid. Returns cell `i`'s
    /// naked candidate mask (the strip's `alts` source).
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn apply_clear(&mut self, i: usize, d0: u8, placed: &mut Placed) -> u16 {
        let (ir, ic) = (bit_r(i), bit_c(i));
        let e = (d0 - 1) as usize;
        // Cell i stops being a clue before we read peer occupancy off `placed`.
        placed.r[e] &= !ir;

        // Phase 1: reopen cells
        // The cleared cell: filled -> empty, its column = its naked candidates.
        // Digit `d` survives iff no still-present peer holds it (`placed[d]` misses
        // i's peer mask) — a banded test per digit, not a per-peer grid scan.
        self.unsolved_r |= ir;
        self.unsolved_c |= ic;
        let prr = peer_mask_r(i);
        let mut cand: u16 = 0;
        for d in 0..9 {
            if !nonzero(placed.r[d] & prr) {
                cand |= 1 << d;
            }
        }
        for d in 0..9 {
            if cand & (1 << d) != 0 {
                self.r[d] |= ir;
                self.c[d] |= ic;
            } else {
                self.r[d] &= !ir;
                self.c[d] &= !ic;
            }
        }

        // Phase 2: reopen peers
        // Its peers: d0 was blocked at every peer by cell i, so its bit there was
        // 0; set it on each empty peer no *other* present peer still holds d0. A
        // clue d0 forbids d0 across its whole unit, i.e. exactly over its peer
        // mask, so the cells still blocked from d0 are the union of the surviving
        // d0 clues' peer masks (cell i is already dropped from `placed`). The
        // reopened peers are i's empty peers outside that union — all band ops.
        let mut blk_r = ZERO;
        let mut blk_c = ZERO;
        for lane in 0..3 {
            let mut w = placed.r[e][lane];
            while w != 0 {
                let q = rm_cell(lane, w.trailing_zeros());
                blk_r |= peer_mask_r(q);
                blk_c |= peer_mask_c(q);
                w &= w - 1;
            }
        }
        self.r[e] |= prr & self.unsolved_r & !blk_r;
        self.c[e] |= peer_mask_c(i) & self.unsolved_c & !blk_c;
        cand
    }

    /// Mirror `b.place(i, d0)` (the strip's revert) onto both views: cell `i`
    /// goes empty -> filled (column cleared), `d0` leaves every peer, and `i`
    /// becomes a clue again in `placed`.
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn apply_place(&mut self, i: usize, d0: u8, placed: &mut Placed) {
        let (ir, ic) = (bit_r(i), bit_c(i));
        let e = (d0 - 1) as usize;
        placed.r[e] |= ir;
        self.unsolved_r &= !ir;
        self.unsolved_c &= !ic;
        for d in 0..9 {
            self.r[d] &= !ir;
            self.c[d] &= !ic;
        }
        self.r[e] &= !peer_mask_r(i);
        self.c[e] &= !peer_mask_c(i);
    }

    /// Place digit `d` (1..=9) at cell `c`: decide the cell in both views, forbid
    /// `d` on its peers in both views. Peer-mask clears are precomputed, so the
    /// two-view sync is just two extra AND-NOTs.
    #[inline(always)]
    fn place(&mut self, cell: usize, d: u8) {
        self.unsolved_r &= !bit_r(cell);
        self.unsolved_c &= !bit_c(cell);
        // SAFETY: d in 1..=9 so d-1 in 0..9.
        unsafe {
            *self.r.get_unchecked_mut((d - 1) as usize) &= !peer_mask_r(cell);
            *self.c.get_unchecked_mut((d - 1) as usize) &= !peer_mask_c(cell);
        }
    }

    /// Candidate digit bitmask (`1 << (digit-1)`) of cell `c`, from the row-major
    /// boards.
    #[inline]
    fn cell_candidates(&self, c: usize) -> u16 {
        bump(7);
        let cb = bit_r(c);
        let mut m = 0u16;
        for d in 0..9 {
            if nonzero(self.r[d] & cb) {
                m |= 1 << d;
            }
        }
        m
    }

    /// THROWAWAY instrumentation: empty cells on this board.
    pub fn count_empties(&self) -> u32 {
        popcnt(self.unsolved_r)
    }

    /// THROWAWAY instrumentation: the exact first-closure a probe pays — clone,
    /// restrict cell `i` to `alts`, propagate ONCE to fixpoint (no branching).
    /// Timing this isolates the closure cost from the branch cost in
    /// `any_alt_solves` (which is closure + branch).
    pub fn probe_closure_only(&self, i: usize, alts: u16) -> bool {
        let mut s = self.clone();
        let (kr, kc) = (bit_r(i), bit_c(i));
        for d in 0..9 {
            if alts & (1 << d) == 0 {
                s.r[d] &= !kr;
                s.c[d] &= !kc;
            }
        }
        matches!(s.propagate(), Prop::Solved)
    }

    /// THROWAWAY instrumentation: how many cells the unrestricted propagation
    /// closure (singles + hidden singles + LC to fixpoint, NO branch) determines.
    pub fn closure_solved(&self) -> u32 {
        let before = popcnt(self.unsolved_r);
        let mut s = self.clone();
        match s.propagate() {
            Prop::Contradiction => before, // dead line — treat as fully resolved
            _ => before - popcnt(s.unsolved_r),
        }
    }

    // --- prober (existence DFS) -------------------------------------------

    /// Naked-single sieve over the row-major boards: which cells admit exactly one
    /// digit (`ones & !twos`) and which admit none (`!ones`).
    #[cfg_attr(not(prof_solver), inline(always))]
    #[cfg_attr(prof_solver, inline(never))]
    fn sieve(&self) -> Sieve {
        let mut ones = ZERO;
        let mut twos = ZERO;
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let b = unsafe { *self.r.get_unchecked(d) };
            twos |= ones & b;
            ones |= b;
        }
        Sieve { ones, twos }
    }

    /// Place a wave of naked singles (cells `singles`) into both views. Returns
    /// false if two singles of the same digit are peers (a contradiction).
    #[cfg_attr(not(prof_solver), inline)]
    #[cfg_attr(prof_solver, inline(never))]
    fn place_singles(&mut self, singles: B) -> bool {
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let group = singles & unsafe { *self.r.get_unchecked(d) };
            if !nonzero(group) {
                continue;
            }
            let mut peers_r = ZERO;
            let mut peers_c = ZERO;
            let mut group_c = ZERO;
            for lane in 0..3 {
                let mut g = group[lane];
                while g != 0 {
                    let cell = rm_cell(lane, g.trailing_zeros());
                    g &= g - 1;
                    peers_r |= peer_mask_r(cell);
                    peers_c |= peer_mask_c(cell);
                    group_c |= bit_c(cell);
                }
            }
            // peer_mask excludes the cell itself, so a group cell lands in the
            // accumulated peers iff another group cell peers it.
            if nonzero(peers_r & group) {
                return false;
            }
            self.unsolved_r &= !group;
            self.unsolved_c &= !group_c;
            self.r[d] &= !peers_r;
            self.c[d] &= !peers_c;
        }
        true
    }

    /// Fused row-major band update: for each band and digit, read the band's live
    /// candidates ONCE and drive both locked-candidates (box↔row) and hidden
    /// singles (rows + boxes — the in-lane units of this view) off it, via the
    /// `BAND_KEEP_OCC` and `SINGLE9` tables. LC clears whole dropped triplets in
    /// both views ([`RM_LC_TRIP`]); each hidden single is placed (which syncs both
    /// views) and the band is re-read so the next unit sees it. Returns whether
    /// anything changed. This is the banded solver's heart — no per-unit scan, no
    /// popcount; the only arithmetic is the occupancy gather and three box shifts.
    #[cfg_attr(prof_solver, inline(never))]
    fn band_update_rm(&mut self) -> bool {
        let mut changed = false;
        for b in 0..3 {
            for d in 0..9 {
                #[cfg(feature = "count")]
                let mut prod = false;
                #[cfg(feature = "count")]
                unsafe {
                    BAND_CTR[2] += 1;
                }
                let mut live = (self.r[d] & self.unsolved_r)[b];
                // Locked candidates: drop the triplets the within-band fixpoint kills.
                let occ = triplet_occ(live);
                let mut dropped = occ as u32 & !BAND_KEEP_OCC[occ];
                if dropped != 0 {
                    changed = true;
                    #[cfg(feature = "count")]
                    {
                        prod = true;
                    }
                    while dropped != 0 {
                        let t = dropped.trailing_zeros() as usize;
                        dropped &= dropped - 1;
                        let (rm, cm) = RM_LC_TRIP[b][t];
                        self.r[d] &= !Simd::from_array(rm);
                        self.c[d] &= !Simd::from_array(cm);
                    }
                    live = (self.r[d] & self.unsolved_r)[b];
                }
                // Hidden singles in the three rows (each a contiguous 9-bit chunk).
                for rr in 0..3 {
                    let s = SINGLE9[((live >> (9 * rr)) & 0x1FF) as usize];
                    if s != 0xFF {
                        self.place(rm_cell(b, 9 * rr + s as u32), d as u8 + 1);
                        changed = true;
                        #[cfg(feature = "count")]
                        {
                            prod = true;
                        }
                        live = (self.r[d] & self.unsolved_r)[b];
                    }
                }
                // Hidden singles in the three boxes (gather each box's 9 bits).
                for k in 0..3 {
                    let bk = ((live >> (3 * k)) & 7)
                        | (((live >> (9 + 3 * k)) & 7) << 3)
                        | (((live >> (18 + 3 * k)) & 7) << 6);
                    let s = SINGLE9[bk as usize] as usize;
                    if s != 0xFF {
                        let bit = (s / 3) * 9 + 3 * k as usize + s % 3;
                        self.place(rm_cell(b, bit as u32), d as u8 + 1);
                        changed = true;
                        #[cfg(feature = "count")]
                        {
                            prod = true;
                        }
                        live = (self.r[d] & self.unsolved_r)[b];
                    }
                }
                #[cfg(feature = "count")]
                if prod {
                    unsafe {
                        BAND_CTR[3] += 1;
                    }
                }
            }
        }
        changed
    }

    /// Fused column-major band update: the transpose of [`band_update_rm`] — same
    /// tables on the col-major bands, giving box↔column LC ([`CM_LC_TRIP`]) and
    /// hidden singles in columns (the in-lane lines of this view). Boxes are
    /// already covered row-major, so only the three columns of each band are swept.
    #[cfg_attr(prof_solver, inline(never))]
    fn band_update_cm(&mut self) -> bool {
        let mut changed = false;
        for b in 0..3 {
            for d in 0..9 {
                #[cfg(feature = "count")]
                let mut prod = false;
                #[cfg(feature = "count")]
                unsafe {
                    BAND_CTR[2] += 1;
                }
                let mut live = (self.c[d] & self.unsolved_c)[b];
                let occ = triplet_occ(live);
                let mut dropped = occ as u32 & !BAND_KEEP_OCC[occ];
                if dropped != 0 {
                    changed = true;
                    #[cfg(feature = "count")]
                    {
                        prod = true;
                    }
                    while dropped != 0 {
                        let t = dropped.trailing_zeros() as usize;
                        dropped &= dropped - 1;
                        let (rm, cm) = CM_LC_TRIP[b][t];
                        self.r[d] &= !Simd::from_array(rm);
                        self.c[d] &= !Simd::from_array(cm);
                    }
                    live = (self.c[d] & self.unsolved_c)[b];
                }
                // Hidden singles in the three columns (each a 9-bit chunk here).
                for cc in 0..3 {
                    let s = SINGLE9[((live >> (9 * cc)) & 0x1FF) as usize];
                    if s != 0xFF {
                        self.place(cm_cell(b, 9 * cc + s as u32), d as u8 + 1);
                        changed = true;
                        #[cfg(feature = "count")]
                        {
                            prod = true;
                        }
                        live = (self.c[d] & self.unsolved_c)[b];
                    }
                }
                #[cfg(feature = "count")]
                if prod {
                    unsafe {
                        BAND_CTR[3] += 1;
                    }
                }
            }
        }
        changed
    }

    /// Run all band propagation to a fixpoint: naked singles (cross-digit sieve)
    /// drained first, then the fused row- and column-major band updates, looping
    /// until nothing changes. Returns the reason it stopped.
    #[cfg_attr(prof_solver, inline(never))]
    fn propagate(&mut self) -> Prop {
        #[cfg(feature = "count")]
        unsafe {
            BAND_CTR[0] += 1;
        }
        loop {
            loop {
                pbump(3);
                let Sieve { ones, twos } = self.sieve();
                if nonzero(self.unsolved_r & !ones) {
                    return Prop::Contradiction;
                }
                if !nonzero(self.unsolved_r) {
                    return Prop::Solved;
                }
                let singles = self.unsolved_r & ones & !twos;
                if !nonzero(singles) {
                    break;
                }
                pbump(4);
                if !self.place_singles(singles) {
                    return Prop::Contradiction;
                }
            }
            #[cfg(feature = "count")]
            unsafe {
                BAND_CTR[1] += 1;
            }
            let mut changed = self.band_update_rm();
            changed |= self.band_update_cm();
            if !changed {
                return Prop::Stuck;
            }
        }
    }

    #[cfg_attr(not(prof_solver), inline)]
    #[cfg_attr(prof_solver, inline(never))]
    fn branch_cell(&self) -> (usize, u16) {
        let mut ones = ZERO;
        let mut twos = ZERO;
        let mut threes = ZERO;
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let b = unsafe { *self.r.get_unchecked(d) };
            threes |= twos & b;
            twos |= ones & b;
            ones |= b;
        }
        let bivalue = self.unsolved_r & twos & !threes;
        let pick = if nonzero(bivalue) { bivalue } else { self.unsolved_r };
        let cell = first_rm(pick);
        let cb = bit_r(cell);
        let mut mask: u16 = 0;
        for d in 0..9 {
            if nonzero(self.r[d] & cb) {
                mask |= 1 << d;
            }
        }
        (cell, mask)
    }

    /// True iff the grid has at least one completion. Propagation runs everything
    /// the dual view makes cheap — naked singles, both orientations of locked
    /// candidates, hidden singles in every unit — to a fixpoint before spending a
    /// branch.
    fn solve_first(&mut self) -> bool {
        pbump(2);
        loop {
            match self.propagate() {
                Prop::Solved => return true,
                Prop::Contradiction => return false,
                Prop::Stuck => {}
            }
            pbump(5);
            let (cell, mask) = self.branch_cell();
            let mut m = mask;
            loop {
                let d = m.trailing_zeros() as u8 + 1;
                m &= m - 1;
                pbump(7);
                if m == 0 {
                    self.place(cell, d);
                    break;
                }
                pbump(6);
                let mut child = self.clone();
                child.place(cell, d);
                if child.solve_first() {
                    return true;
                }
            }
        }
    }

    /// Uniqueness gate: restrict cell `i` to the alternate digits `alts` and ask
    /// whether any completion exists — i.e. stripping `i` made the puzzle
    /// non-unique. Consumes a clone, so the base is untouched for the baseline.
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn any_alt_solves(&self, i: usize, alts: u16) -> bool {
        pbump(0);
        let mut s = self.clone();
        let (kr, kc) = (bit_r(i), bit_c(i));
        for d in 0..9 {
            if alts & (1 << d) == 0 {
                s.r[d] &= !kr;
                s.c[d] &= !kc;
            }
        }
        #[cfg(feature = "count")]
        let (n0, g0, empties) = unsafe { (PCTR[2], PCTR[5], popcnt(self.unsolved_r) as u16) };
        let r = s.solve_first();
        if r {
            pbump(1);
        }
        #[cfg(feature = "count")]
        unsafe {
            (*core::ptr::addr_of_mut!(ALT_STATS)).push(AltStat {
                empties,
                nodes: (PCTR[2] - n0) as u32,
                guesses: (PCTR[5] - g0) as u32,
                nonunique: r,
            });
        }
        r
    }

    // --- baseline technique engine ----------------------------------------
    //
    // The baseline is the spec oracle: techniques stay discrete, gated by
    // `allowed`, in difficulty order, with per-kind firing counts. It reads only
    // the row-major view (columns straddle lanes there, but baseline isn't the
    // peak hot path and `exactly_one`/`nonzero` handle the cross-lane case); the
    // column-major view rides along in the clone unused.

    /// Solve with the `allowed` toolbox, easiest-first, tallying which kinds
    /// fired. Naked singles drain in bit-parallel waves; the rarer harder
    /// techniques apply one step at a time. Clones the base, so the prober can
    /// reuse it.
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn baseline(&self, allowed: Mask) -> Outcome {
        bump(0);
        let mut bb = self.clone();
        let mut counts = [0u16; NUM];
        let ns = allowed & (1 << NAKED_SINGLE) != 0;
        loop {
            if ns {
                let n = bb.drain_naked_singles();
                if n > 0 {
                    counts[NAKED_SINGLE] = counts[NAKED_SINGLE].saturating_add(n);
                    continue;
                }
            }
            if !nonzero(bb.unsolved_r) {
                return Outcome { solved: true, counts };
            }
            match bb.step_harder(allowed) {
                Some(k) => counts[k] = counts[k].saturating_add(1),
                None => return Outcome { solved: false, counts },
            }
        }
    }

    /// Place every naked single (cell with exactly one candidate) in waves until
    /// none remain; returns how many were placed.
    #[inline]
    fn drain_naked_singles(&mut self) -> u16 {
        let mut total = 0u16;
        loop {
            bump(1);
            let Sieve { ones, twos } = self.sieve();
            let singles = self.unsolved_r & ones & !twos;
            if !nonzero(singles) {
                return total;
            }
            let n = popcnt(singles) as u16;
            // On a uniquely-solvable board (the only kind baseline sees) naked
            // singles are forced moves and never conflict, so place_singles holds.
            if !self.place_singles(singles) {
                return total;
            }
            total = total.saturating_add(n);
        }
    }

    /// Apply the first applicable harder technique (hidden single up to hidden
    /// quad, in difficulty order, gated by `allowed`); return its kind index.
    fn step_harder(&mut self, allowed: Mask) -> Option<usize> {
        if allowed & (1 << HIDDEN_SINGLE) != 0 && self.hidden_single() {
            return Some(HIDDEN_SINGLE);
        }
        if allowed & (1 << LC_POINTING) != 0 && self.lc_pointing() {
            return Some(LC_POINTING);
        }
        if allowed & (1 << LC_CLAIMING) != 0 && self.lc_claiming() {
            return Some(LC_CLAIMING);
        }
        if allowed & (1 << NAKED_PAIR) != 0 && self.naked_subset(2) {
            return Some(NAKED_PAIR);
        }
        if allowed & (1 << HIDDEN_PAIR) != 0 && self.hidden_subset(2) {
            return Some(HIDDEN_PAIR);
        }
        if allowed & (1 << NAKED_TRIPLE) != 0 && self.naked_subset(3) {
            return Some(NAKED_TRIPLE);
        }
        if allowed & (1 << HIDDEN_TRIPLE) != 0 && self.hidden_subset(3) {
            return Some(HIDDEN_TRIPLE);
        }
        if allowed & (1 << NAKED_QUAD) != 0 && self.naked_subset(4) {
            return Some(NAKED_QUAD);
        }
        if allowed & (1 << HIDDEN_QUAD) != 0 && self.hidden_subset(4) {
            return Some(HIDDEN_QUAD);
        }
        None
    }

    /// Hidden single: a digit with exactly one placeable empty cell in a unit.
    /// Precompute `r[d] & unsolved_r` once per digit so the 27×9 unit scan is one
    /// AND + `exactly_one` per pair. Returns at the first placement, before any
    /// board mutation.
    fn hidden_single(&mut self) -> bool {
        bump(2);
        let mut bd = [ZERO; 9];
        for d in 0..9 {
            bd[d] = self.r[d] & self.unsolved_r;
        }
        for u in 0..27 {
            let um = unit_mask_r(u);
            for d in 0..9 {
                let pos = bd[d] & um;
                if exactly_one(pos) {
                    self.place(first_rm(pos), d as u8 + 1);
                    return true;
                }
            }
        }
        false
    }

    /// Locked candidates (pointing): a digit confined to one line within a box
    /// is eliminated from the rest of that line.
    fn lc_pointing(&mut self) -> bool {
        bump(3);
        for b in 0..9 {
            let bm = unit_mask_r(18 + b);
            let br = (b / 3) * 3;
            let bc = (b % 3) * 3;
            for d in 0..9 {
                let pos = self.r[d] & bm & self.unsolved_r;
                if !at_least_two(pos) {
                    continue;
                }
                // rows of this box
                for rr in br..br + 3 {
                    let rm = unit_mask_r(rr);
                    if nonzero(pos & !rm) {
                        continue; // not all in this row
                    }
                    let targets = self.r[d] & rm & !bm & self.unsolved_r;
                    if nonzero(targets) {
                        self.eliminate_both(d, targets);
                        return true;
                    }
                }
                // cols of this box
                for cc in bc..bc + 3 {
                    let cm = unit_mask_r(9 + cc);
                    if nonzero(pos & !cm) {
                        continue;
                    }
                    let targets = self.r[d] & cm & !bm & self.unsolved_r;
                    if nonzero(targets) {
                        self.eliminate_both(d, targets);
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Locked candidates (claiming): a digit confined to one box within a line
    /// is eliminated from the rest of that box.
    fn lc_claiming(&mut self) -> bool {
        bump(4);
        for line in 0..18 {
            // 0..9 rows, 9..18 cols
            let lm = unit_mask_r(line);
            for d in 0..9 {
                let pos = self.r[d] & lm & self.unsolved_r;
                if !at_least_two(pos) {
                    continue;
                }
                let first = first_rm(pos);
                let b = (first / 9 / 3) * 3 + (first % 9) / 3;
                let bm = unit_mask_r(18 + b);
                if nonzero(pos & !bm) {
                    continue; // not all in one box
                }
                let targets = self.r[d] & bm & !lm & self.unsolved_r;
                if nonzero(targets) {
                    self.eliminate_both(d, targets);
                    return true;
                }
            }
        }
        false
    }

    /// Clear digit `d` (0-based) from the row-major target cells `targets` in both
    /// views — the baseline's elimination primitive, mirroring a row-major mask
    /// into the column-major copy cell by cell.
    #[inline]
    fn eliminate_both(&mut self, d: usize, targets: B) {
        self.r[d] &= !targets;
        for lane in 0..3 {
            let mut g = targets[lane];
            while g != 0 {
                let cell = rm_cell(lane, g.trailing_zeros());
                g &= g - 1;
                self.c[d] &= !bit_c(cell);
            }
        }
    }

    /// Naked subset of `size`: `size` cells in a unit whose candidate union is
    /// exactly `size` digits — those digits leave the other cells of the unit.
    fn naked_subset(&mut self, size: usize) -> bool {
        bump(5);
        for u in 0..27 {
            let cells = UNIT_CELLS[u];
            // candidate cells: empty, 2..=size candidates.
            let mut cand_cells: [usize; 9] = [0; 9];
            let mut cand_masks: [u16; 9] = [0; 9];
            let mut n = 0usize;
            for &c in &cells {
                if nonzero(self.unsolved_r & bit_r(c)) {
                    let m = self.cell_candidates(c);
                    let pc = m.count_ones() as usize;
                    if pc >= 2 && pc <= size {
                        cand_cells[n] = c;
                        cand_masks[n] = m;
                        n += 1;
                    }
                }
            }
            if n < size {
                continue;
            }
            let idx: [usize; 9] = core::array::from_fn(|k| k);
            let mut applied = false;
            for_each_combination(&idx[..n], size, |combo| {
                let union: u16 = combo.iter().map(|&k| cand_masks[k]).fold(0, |a, x| a | x);
                if union.count_ones() as usize != size {
                    return true; // keep searching
                }
                // eliminate union digits from the OTHER cells of the unit.
                let mut did = false;
                for &c in &cells {
                    if !nonzero(self.unsolved_r & bit_r(c)) {
                        continue;
                    }
                    if combo.iter().any(|&k| cand_cells[k] == c) {
                        continue;
                    }
                    let rm = self.cell_candidates(c) & union;
                    if rm != 0 {
                        for d in 0..9 {
                            if rm & (1 << d) != 0 {
                                self.r[d] &= !bit_r(c);
                                self.c[d] &= !bit_c(c);
                            }
                        }
                        did = true;
                    }
                }
                applied = did;
                !did // stop once we eliminated something
            });
            if applied {
                return true;
            }
        }
        false
    }

    /// Hidden subset of `size`: `size` digits confined to the same `size` cells
    /// of a unit — the other digits leave those cells.
    fn hidden_subset(&mut self, size: usize) -> bool {
        bump(6);
        for u in 0..27 {
            let cells = UNIT_CELLS[u];
            // position mask (over the 9 unit-cell indices) per digit.
            let mut pos: [u16; 9] = [0; 9];
            let mut digits: [usize; 9] = [0; 9];
            let mut n = 0usize;
            for d in 0..9 {
                let mut p: u16 = 0;
                for (k, &c) in cells.iter().enumerate() {
                    if nonzero(self.r[d] & bit_r(c) & self.unsolved_r) {
                        p |= 1 << k;
                    }
                }
                let pc = p.count_ones() as usize;
                if pc >= 2 && pc <= size {
                    pos[d] = p;
                    digits[n] = d;
                    n += 1;
                }
            }
            if n < size {
                continue;
            }
            let idx: [usize; 9] = core::array::from_fn(|k| k);
            let mut applied = false;
            for_each_combination(&idx[..n], size, |combo| {
                let union: u16 = combo.iter().map(|&k| pos[digits[k]]).fold(0, |a, x| a | x);
                if union.count_ones() as usize != size {
                    return true;
                }
                let keep: u16 = combo.iter().map(|&k| 1u16 << digits[k]).fold(0, |a, x| a | x);
                let mut did = false;
                for k in 0..9 {
                    if union & (1 << k) == 0 {
                        continue;
                    }
                    let c = cells[k];
                    let rm = self.cell_candidates(c) & !keep;
                    if rm != 0 {
                        for d in 0..9 {
                            if rm & (1 << d) != 0 {
                                self.r[d] &= !bit_r(c);
                                self.c[d] &= !bit_c(c);
                            }
                        }
                        did = true;
                    }
                }
                applied = did;
                !did
            });
            if applied {
                return true;
            }
        }
        false
    }
}
