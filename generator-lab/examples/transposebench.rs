//! Bench: row-major -> column-major banded transpose of a sudoku digit board,
//! across two within-band bit layouts.
//!
//! The DualView `BitBoard` keeps BOTH bandings live and syncs them on every
//! mutation because the cost of transposing on demand was never measured. This
//! quantifies that cost, and asks whether GFNI (`gf2p8affineqb`) ever beats scalar.
//!
//! GFNI transposes an 8x8 bitmatrix held one-row-per-byte. The cost around it is
//! the pack/repack between that byte-aligned form and the band layout. Two layouts:
//!
//!   9-9-9 (current): a band's 3 rows are contiguous 9-bit fields (stride 9). The
//!     hot `SINGLE9`/`OCC3` lookups need this. GFNI's pack/repack must re-stride
//!     9<->8, which is the scaffolding that makes GFNI lose. Variants:
//!       ref / swar / gfni (scalar re-stride) / gfniP (PEXT pack + PDEP repack).
//!
//!   8-8-8-1-1-1 ("byte-banded"): a band's 3 rows put cols 0-7 in bytes 0,1,2 and
//!     the three 9th-column cells in bits 24,25,26. Rows are byte-aligned, so the
//!     GFNI pack/repack collapse to a few shifts. This BREAKS `SINGLE9`/LC (rows no
//!     longer contiguous 9-bit) -- but the gather-free SIMT prober uses neither
//!     (`one_bit` over masks is order-agnostic), so it is viable there. Variants:
//!       refB / swarB / gfniB.
//!
//! Every variant is verified bit-identical against its layout's scalar oracle and
//! a RM->CM->RM round-trip before timing.
//!
//! Usage: cargo run --release -p generator-lab --example transposebench -- [--boards B] [--iters I]

use generator_lab::rng::Rng;
use std::hint::black_box;
use std::time::Instant;

type Digit = [u32; 3];
type Board = [Digit; 9];

// --- 9-9-9 layout maps (current, = bb::layout) --------------------------------
const fn rm_lane(r: usize) -> usize {
    r / 3
}
const fn rm_bit(r: usize, c: usize) -> usize {
    (r % 3) * 9 + c
}
const fn cm_lane(c: usize) -> usize {
    c / 3
}
const fn cm_bit(r: usize, c: usize) -> usize {
    (c % 3) * 9 + r
}
fn cell_of_999(band: usize, bit: usize) -> (usize, usize) {
    (band * 3 + bit / 9, bit % 9)
}

// --- 8-8-8-1-1-1 ("byte-banded") layout maps ----------------------------------
// Within a band: cols 0-7 of the 3 rows -> bytes 0,1,2; the three col-8 cells ->
// bits 24,25,26. Rows become byte-aligned at the cost of 9-bit-field contiguity.
const fn bb_rm_lane(r: usize) -> usize {
    r / 3
}
const fn bb_rm_bit(r: usize, c: usize) -> usize {
    if c < 8 { (r % 3) * 8 + c } else { 24 + (r % 3) }
}
const fn bb_cm_lane(c: usize) -> usize {
    c / 3
}
const fn bb_cm_bit(r: usize, c: usize) -> usize {
    if r < 8 { (c % 3) * 8 + r } else { 24 + (c % 3) }
}
fn cell_of_bb(band: usize, bit: usize) -> (usize, usize) {
    if bit < 24 { (band * 3 + bit / 8, bit % 8) } else { (band * 3 + (bit - 24), 8) }
}

// --- scalar oracles -----------------------------------------------------------
fn transpose_ref(src: &Digit) -> Digit {
    let mut dst = [0u32; 3];
    for r in 0..9 {
        for c in 0..9 {
            if (src[rm_lane(r)] >> rm_bit(r, c)) & 1 != 0 {
                dst[cm_lane(c)] |= 1 << cm_bit(r, c);
            }
        }
    }
    dst
}
fn transpose_ref_bb(src: &Digit) -> Digit {
    let mut dst = [0u32; 3];
    for r in 0..9 {
        for c in 0..9 {
            if (src[bb_rm_lane(r)] >> bb_rm_bit(r, c)) & 1 != 0 {
                dst[bb_cm_lane(c)] |= 1 << bb_cm_bit(r, c);
            }
        }
    }
    dst
}

// --- SWAR byte-spread table (layout-generic) ----------------------------------
/// `tbl[band][byte][value]` = the CM contribution of the set bits in `value`
/// sitting at byte `byte` of source band `band`. The full transpose AND re-stride
/// are baked in, so a transpose is 12 table loads OR'd -- no separate repack.
struct Swar {
    tbl: Vec<[[[u32; 3]; 256]; 4]>,
}
impl Swar {
    fn build(cell_of: impl Fn(usize, usize) -> (usize, usize), dst_of: impl Fn(usize, usize) -> (usize, usize)) -> Self {
        let mut tbl = vec![[[[0u32; 3]; 256]; 4]; 3];
        for band in 0..3 {
            for byte in 0..4 {
                for v in 0..256usize {
                    let mut out = [0u32; 3];
                    for b in 0..8 {
                        if v >> b & 1 == 0 {
                            continue;
                        }
                        let bit = byte * 8 + b;
                        if bit >= 27 {
                            continue;
                        }
                        let (r, c) = cell_of(band, bit);
                        let (lane, dbit) = dst_of(r, c);
                        out[lane] |= 1 << dbit;
                    }
                    tbl[band][byte][v] = out;
                }
            }
        }
        Swar { tbl }
    }
    #[inline]
    fn transpose(&self, src: &Digit) -> Digit {
        let mut d = [0u32; 3];
        for band in 0..3 {
            let x = src[band];
            let t = &self.tbl[band];
            for byte in 0..4 {
                let c = t[byte][((x >> (8 * byte)) & 0xFF) as usize];
                d[0] |= c[0];
                d[1] |= c[1];
                d[2] |= c[2];
            }
        }
        d
    }
}

// --- GFNI ---------------------------------------------------------------------
#[cfg(target_arch = "x86_64")]
pub mod gfni {
    use super::*;
    use std::arch::x86_64::*;

    #[derive(Clone, Copy)]
    pub struct Cfg {
        onehot: u64,
        swap: bool,
        rev: bool,
    }

    #[target_feature(enable = "gfni,sse2")]
    unsafe fn affine(s1: u64, mat: u64) -> u64 {
        let t = _mm_gf2p8affine_epi64_epi8(_mm_set1_epi64x(s1 as i64), _mm_set1_epi64x(mat as i64), 0);
        _mm_cvtsi128_si64(t) as u64
    }
    #[inline]
    fn revbytes(x: u64) -> u64 {
        let mut o = 0u64;
        for b in 0..8 {
            o |= ((((x >> (8 * b)) & 0xFF) as u8).reverse_bits() as u64) << (8 * b);
        }
        o
    }
    #[target_feature(enable = "gfni,sse2")]
    unsafe fn raw(rows: u64, cfg: Cfg) -> u64 {
        let (s1, mat) = if cfg.swap { (rows, cfg.onehot) } else { (cfg.onehot, rows) };
        let r = unsafe { affine(s1, mat) };
        if cfg.rev { revbytes(r) } else { r }
    }
    fn t8_ref(x: u64) -> u64 {
        let mut o = 0u64;
        for r in 0..8 {
            for c in 0..8 {
                if x >> (8 * r + c) & 1 != 0 {
                    o |= 1 << (8 * c + r);
                }
            }
        }
        o
    }
    impl Cfg {
        /// Settle GFNI's bit-reflection convention empirically: find the operand
        /// order / one-hot / output-reversal that makes `raw` a true 8x8 transpose.
        pub fn find() -> Option<Cfg> {
            let onehots = [0x8040201008040201u64, 0x0102040810204080u64];
            let mut rng = Rng::from_seed(0xC0FFEE);
            let samples: Vec<u64> = (0..512).map(|_| rng.next_u64()).collect();
            for &onehot in &onehots {
                for swap in [false, true] {
                    for rev in [false, true] {
                        let cfg = Cfg { onehot, swap, rev };
                        if samples.iter().all(|&x| unsafe { raw(x, cfg) } == t8_ref(x)) {
                            return Some(cfg);
                        }
                    }
                }
            }
            None
        }
    }

    /// Shared 9th-row + 9th-column edge handling for the 9-9-9 variants (the cells
    /// outside the 8x8 core). Scalar either way; identical for gfni and gfniP.
    #[inline]
    fn edges_999(src: &Digit, dst: &mut Digit) {
        let row8 = (src[2] >> 18) & 0x1FF; // row 8 = band 2, field 2 (bits 18..26)
        for c in 0..9 {
            if row8 >> c & 1 != 0 {
                dst[cm_lane(c)] |= 1 << cm_bit(8, c);
            }
        }
        for r in 0..9 {
            if src[rm_lane(r)] >> ((r % 3) * 9 + 8) & 1 != 0 {
                dst[2] |= 1 << (18 + r); // col 8 -> CM band 2, bit 18+r
            }
        }
    }

    /// 9-9-9 with a scalar 9<->8 re-stride (the original GFNI variant).
    #[target_feature(enable = "gfni,sse2")]
    pub unsafe fn transpose_999(src: &Digit, cfg: Cfg) -> Digit {
        let row = |r: usize| (src[rm_lane(r)] >> ((r % 3) * 9)) & 0x1FF;
        let mut core = 0u64;
        for r in 0..8 {
            core |= ((row(r) & 0xFF) as u64) << (8 * r);
        }
        let coret = unsafe { raw(core, cfg) };
        let mut dst = [0u32; 3];
        for c in 0..8 {
            dst[cm_lane(c)] |= (((coret >> (8 * c)) & 0xFF) as u32) << ((c % 3) * 9);
        }
        edges_999(src, &mut dst);
        dst
    }

    /// 9-9-9 with PEXT pack + PDEP repack (the 9<->8 re-stride in hardware).
    #[target_feature(enable = "gfni,bmi2,sse2")]
    pub unsafe fn transpose_999_pdep(src: &Digit, cfg: Cfg) -> Digit {
        const M: u64 = 0xFF | (0xFF << 9) | (0xFF << 18); // cols 0-7 of the 3 rows
        // pack: each band's cols 0-7 -> contiguous bytes; assemble rows 0-7.
        let p0 = _pext_u64(src[0] as u64, M);
        let p1 = _pext_u64(src[1] as u64, M);
        let p2 = _pext_u64(src[2] as u64, M);
        let core = p0 | (p1 << 24) | ((p2 & 0xFFFF) << 48);
        let coret = unsafe { raw(core, cfg) };
        // repack: each band's 3 columns (contiguous bytes) -> 9-bit fields.
        let mut dst = [0u32; 3];
        dst[0] = _pdep_u64(coret & 0xFF_FFFF, M) as u32;
        dst[1] = _pdep_u64((coret >> 24) & 0xFF_FFFF, M) as u32;
        dst[2] = _pdep_u64((coret >> 48) & 0xFFFF, M) as u32;
        edges_999(src, &mut dst);
        dst
    }

    /// 8-8-8-1-1-1: rows are byte-aligned, so pack/repack are a few shifts.
    #[target_feature(enable = "gfni,sse2")]
    pub unsafe fn transpose_bb(src: &Digit, cfg: Cfg) -> Digit {
        // pack: row r's cols 0-7 = byte (r%3) of band r/3 -> already byte-aligned.
        let core = (src[0] as u64 & 0xFF_FFFF)
            | ((src[1] as u64 & 0xFF_FFFF) << 24)
            | ((src[2] as u64 & 0xFFFF) << 48); // rows 6,7
        let coret = unsafe { raw(core, cfg) };
        // repack: col c's rows 0-7 = byte c of coret -> byte (c%3) of CM band c/3.
        let mut dst = [0u32; 3];
        dst[0] = (coret & 0xFF_FFFF) as u32; // cols 0,1,2
        dst[1] = ((coret >> 24) & 0xFF_FFFF) as u32; // cols 3,4,5
        dst[2] = ((coret >> 48) & 0xFFFF) as u32; // cols 6,7
        // edges: row 8 (cols 0-7 = band2 byte2) and col 8 (rows 0-7 = bits 24+r%3).
        let row8 = (src[2] >> 16) & 0xFF;
        for c in 0..8 {
            if row8 >> c & 1 != 0 {
                dst[bb_cm_lane(c)] |= 1 << bb_cm_bit(8, c);
            }
        }
        for r in 0..8 {
            if src[bb_rm_lane(r)] >> (24 + r % 3) & 1 != 0 {
                dst[2] |= 1 << bb_cm_bit(r, 8);
            }
        }
        if src[2] >> 26 & 1 != 0 {
            dst[2] |= 1 << bb_cm_bit(8, 8); // corner (8,8)
        }
        dst
    }
}

// --- harness ------------------------------------------------------------------
fn rand_digit(rng: &mut Rng) -> Digit {
    let mut d = [0u32; 3];
    for band in 0..3 {
        let mut x = 0u32;
        for bit in 0..27 {
            if rng.range(100) < 45 {
                x |= 1 << bit;
            }
        }
        d[band] = x;
    }
    d
}
fn rand_board(rng: &mut Rng) -> Board {
    core::array::from_fn(|_| rand_digit(rng))
}
#[inline(always)]
fn fold(t: Digit) -> u64 {
    t[0] as u64 ^ (t[1] as u64) << 13 ^ (t[2] as u64) << 26
}

fn main() {
    let mut boards_n = 4000usize;
    let mut iters = 1500usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--boards" => boards_n = it.next().and_then(|s| s.parse().ok()).unwrap_or(boards_n),
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            _ => {}
        }
    }

    let mut rng = Rng::from_seed(1);
    let boards: Vec<Board> = (0..boards_n).map(|_| rand_board(&mut rng)).collect();
    let digits: Vec<Digit> = boards.iter().flatten().copied().collect();
    let swar = Swar::build(cell_of_999, |r, c| (cm_lane(c), cm_bit(r, c)));
    let swar_bb = Swar::build(cell_of_bb, |r, c| (bb_cm_lane(c), bb_cm_bit(r, c)));
    let cfg = gfni::Cfg::find();

    // --- correctness: each variant matches its layout's oracle + round-trips.
    let mut bad = 0u64;
    for d in &digits {
        let r = transpose_ref(d);
        bad += (swar.transpose(d) != r) as u64;
        bad += (transpose_ref(&r) != *d) as u64;
        let rb = transpose_ref_bb(d);
        bad += (swar_bb.transpose(d) != rb) as u64;
        bad += (transpose_ref_bb(&rb) != *d) as u64;
        if let Some(c) = cfg {
            bad += (unsafe { gfni::transpose_999(d, c) } != r) as u64;
            bad += (unsafe { gfni::transpose_999_pdep(d, c) } != r) as u64;
            bad += (unsafe { gfni::transpose_bb(d, c) } != rb) as u64;
        }
    }
    println!(
        "verify: {} digits | mismatches = {} | gfni = {}\n",
        digits.len(),
        bad,
        if cfg.is_some() { "ok" } else { "NO CONVENTION" }
    );
    if bad != 0 {
        eprintln!("CORRECTNESS FAILURE -- timings meaningless");
    }

    let n = (iters * digits.len()) as f64;
    macro_rules! bench {
        ($name:expr, $tr:expr) => {{
            let f = $tr;
            black_box(f(&digits[0]));
            let t0 = Instant::now();
            let mut acc = 0u64;
            for _ in 0..iters {
                for d in black_box(&digits) {
                    acc ^= fold(f(black_box(d)));
                }
            }
            let el = t0.elapsed();
            black_box(acc);
            let per = el.as_secs_f64() * 1e9 / n;
            println!("  {:7}: {:6.2} ns/digit   {:7.2} ns/board", $name, per, per * 9.0);
        }};
    }

    println!("timing ({} digits, {} iters):", digits.len(), iters);
    println!(" 9-9-9 (current layout):");
    bench!("ref", |d: &Digit| transpose_ref(d));
    bench!("swar", |d: &Digit| swar.transpose(d));
    if let Some(c) = cfg {
        bench!("gfni", |d: &Digit| unsafe { gfni::transpose_999(d, c) });
        bench!("gfniP", |d: &Digit| unsafe { gfni::transpose_999_pdep(d, c) });
    }
    println!(" 8-8-8-1-1-1 (byte-banded):");
    bench!("refB", |d: &Digit| transpose_ref_bb(d));
    bench!("swarB", |d: &Digit| swar_bb.transpose(d));
    if let Some(c) = cfg {
        bench!("gfniB", |d: &Digit| unsafe { gfni::transpose_bb(d, c) });
    }
}
