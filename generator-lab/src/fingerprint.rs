//! The shared FNV-1a fingerprint primitives every generator path folds puzzles
//! with — the cross-backend determinism guard and the bench/equivalence fps.

use crate::repr::{CELLS, DigitGrid};

/// FNV-1a 64-bit offset basis — the seed for every puzzle fingerprint in the
/// crate (the cross-backend determinism guard and the bench/equivalence fps).
pub const FNV_OFFSET: u64 = 0xcbf29ce484222325;
/// FNV-1a 64-bit prime.
pub const FNV_PRIME: u64 = 0x100000001b3;

/// Fold a puzzle's 81 raw cell bytes (`1..=9`, `0` = empty) into a rolling FNV-1a
/// fingerprint. The shared primitive behind the cross-backend determinism guard
/// (`run_attempts` / `determinism_fp`) and [`grid_fp`].
#[inline]
pub fn fnv_fold_cells(fp: &mut u64, cells: &[u8; CELLS]) {
    for &d in cells {
        *fp ^= d as u64;
        *fp = fp.wrapping_mul(FNV_PRIME);
    }
}

/// The FNV-1a fingerprint of one grid's 81 cells — a per-puzzle id. XOR-combine these for
/// an order-independent fingerprint over a produced *set* of puzzles (e.g. a bench A/B that
/// two builds emit the same puzzles, regardless of the warp's completion order).
pub fn grid_fp(grid: &DigitGrid) -> u64 {
    let mut fp = FNV_OFFSET;
    fnv_fold_cells(&mut fp, &grid.cell_bytes());
    fp
}
