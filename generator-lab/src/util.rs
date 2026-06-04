//! k-combination iterator, copied from `core::util` (specialized loops for the
//! small known k — subset techniques call this on the hot path); plus the shared
//! FNV-1a fingerprint primitives every generator path folds puzzles with.

use crate::grid::{CELLS, Digit};

/// FNV-1a 64-bit offset basis — the seed for every puzzle fingerprint in the
/// crate (the cross-backend determinism guard and the bench/equivalence fps).
pub const FNV_OFFSET: u64 = 0xcbf29ce484222325;
/// FNV-1a 64-bit prime.
pub const FNV_PRIME: u64 = 0x100000001b3;

/// Fold a puzzle's 81 cell digits into a rolling FNV-1a fingerprint. Used by both
/// the sequential `run_attempts` and the warp's per-lane `finalize`, so their
/// fingerprints are identical by construction (the warp equivalence test relies
/// on it).
#[inline]
pub fn fnv_fold_cells(fp: &mut u64, cells: &[Digit; CELLS]) {
    for &d in cells {
        *fp ^= d as u64;
        *fp = fp.wrapping_mul(FNV_PRIME);
    }
}

pub fn for_each_combination<T: Copy, F: FnMut(&[T]) -> bool>(
    items: &[T],
    k: usize,
    mut f: F,
) -> bool {
    let n = items.len();
    if k == 0 || n < k {
        return true;
    }
    match k {
        2 => {
            for i in 0..n {
                for j in i + 1..n {
                    if !f(&[items[i], items[j]]) {
                        return false;
                    }
                }
            }
        }
        3 => {
            for i in 0..n {
                for j in i + 1..n {
                    for l in j + 1..n {
                        if !f(&[items[i], items[j], items[l]]) {
                            return false;
                        }
                    }
                }
            }
        }
        4 => {
            for i in 0..n {
                for j in i + 1..n {
                    for l in j + 1..n {
                        for m in l + 1..n {
                            if !f(&[items[i], items[j], items[l], items[m]]) {
                                return false;
                            }
                        }
                    }
                }
            }
        }
        _ => unreachable!("solver-lab only uses k in 2..=4 (subset techniques)"),
    }
    true
}
