//! Units and the peer relation — the grid's row/column/box structure.
//!
//! [`PEERS`] is the per-cell conflict set (all the board representation needs to
//! maintain marks); [`UNITS`] is the 27 units as cell sequences, which the
//! unit-scan techniques (hidden single, subsets, ...) walk. A cell's peers are
//! exactly the union of the three units it belongs to, minus itself.

use super::{CELLS, CellIdx};

/// For each cell, its 20 peers: the other cells sharing its row, column, or box.
pub const PEERS: [[CellIdx; 20]; CELLS] = build_peers();

/// For each cell, the 81-bit mask of its 20 peers (bit `p` set iff `p` shares the
/// cell's row, column, or box). The bitset form of [`PEERS`] for the wing family's
/// "does `s` see `c`" tests and common-peer intersection: a bit test / AND-and-walk
/// instead of a linear `contains` scan over the 20-element peer list per cell.
pub const PEER_MASK: [u128; CELLS] = build_peer_mask();

/// The 27 units — the 9 rows, then the 9 columns, then the 9 boxes — each as its
/// nine cell indices (row-major). The sequences a unit-scan technique iterates;
/// `UNITS[0..9]` are rows, `[9..18]` columns, `[18..27]` boxes.
pub const UNITS: [[CellIdx; 9]; 27] = build_units();

const fn build_units() -> [[CellIdx; 9]; 27] {
    let mut units = [[0usize; 9]; 27];
    let mut k = 0;
    while k < 9 {
        let mut i = 0;
        while i < 9 {
            units[k][i] = k * 9 + i; // row k: cells (k, 0..9)
            units[9 + k][i] = i * 9 + k; // column k: cells (0..9, k)
            // box k: top-left at (3*(k/3), 3*(k%3)), cells in row-major order.
            let br = (k / 3) * 3;
            let bc = (k % 3) * 3;
            units[18 + k][i] = (br + i / 3) * 9 + (bc + i % 3);
            i += 1;
        }
        k += 1;
    }
    units
}

const fn build_peer_mask() -> [u128; CELLS] {
    let mut masks = [0u128; CELLS];
    let mut i = 0;
    while i < CELLS {
        let mut k = 0;
        while k < 20 {
            masks[i] |= 1u128 << PEERS[i][k];
            k += 1;
        }
        i += 1;
    }
    masks
}

const fn build_peers() -> [[CellIdx; 20]; CELLS] {
    let mut peers = [[0usize; 20]; 81];
    let mut i = 0;
    while i < 81 {
        let row = i / 9;
        let col = i % 9;
        let br = (row / 3) * 3;
        let bc = (col / 3) * 3;
        let mut k = 0;
        let mut j = 0;
        while j < 81 {
            if j != i {
                let jr = j / 9;
                let jc = j % 9;
                let same_row = jr == row;
                let same_col = jc == col;
                let same_box = jr >= br && jr < br + 3 && jc >= bc && jc < bc + 3;
                if same_row || same_col || same_box {
                    peers[i][k] = j;
                    k += 1;
                }
            }
            j += 1;
        }
        i += 1;
    }
    peers
}
