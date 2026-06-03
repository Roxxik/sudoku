//! Minimal W=1 integrated prototype of the proposed SIMT control flow, to
//! de-risk the integration unknowns BEFORE committing to a vector width. It is
//! deliberately scalar (no packing win); what it validates is that the rewritten
//! control flow and data layout are CORRECT and behave as the cost model assumed:
//!
//!   - SINGLE-LAYOUT state: row-major bands only (r[9][3] + unsolved[3] = 30 u32),
//!     no column-major view. (AoSoA at W=1 degenerates to this contiguous block.)
//!   - GATHER-FREE sweep: naked singles via the cross-digit sieve; hidden singles
//!     in rows+boxes by pure ALU exactly-one (no SINGLE9 table). NO locked
//!     candidates, NO column hidden singles.
//!   - SMEAR place_singles: the validated band-formula peer union (one smear, no
//!     col, no per-cell gather) — the form that only pays off single-layout.
//!   - EXPLICIT-STACK DFS (no recursion): the form a warp lane runs, with a clone
//!     of the 30-word state pushed per branch.
//!
//! It runs the real strip loop and, at every uniqueness query, runs this prober on
//! the same board and checks its yes/no verdict against the shipped dual-layout
//! prober (`any_alt_solves`). Verdicts MUST match 100% (existence is layout- and
//! technique-independent); the node-count ratio should land near the 1.17x
//! single-layout inflation we measured. A mismatch means the rewrite is unsound.
//!
//! Usage: cargo run --release -p generator-lab --example miniprober -- [--attempts N=8000] [--mode train|drill]

use generator_lab::bb::{BitBoard, Placed};
use generator_lab::generator::random_full_grid;
use generator_lab::grid::{CELLS, Digit, PEERS, digit_to_bit};
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;

const fn rm_lane(c: usize) -> usize {
    (c / 9) / 3
}
const fn rm_bit(c: usize) -> usize {
    ((c / 9) % 3) * 9 + c % 9
}
const fn rm_cell(lane: usize, bit: u32) -> usize {
    let b = bit as usize;
    (3 * lane + b / 9) * 9 + b % 9
}

const BOX_CELLS: [u32; 3] = {
    let mut t = [0u32; 3];
    let mut k = 0;
    while k < 3 {
        let c3 = 0b111u32 << (3 * k);
        t[k] = c3 | (c3 << 9) | (c3 << 18);
        k += 1;
    }
    t
};

/// peer_mask_r[cell] = 20 peers as 3 row-major band words (self excluded).
fn peer_table() -> Vec<[u32; 3]> {
    let mut t = vec![[0u32; 3]; CELLS];
    for c in 0..CELLS {
        for &q in &PEERS[c] {
            t[c][rm_lane(q)] |= 1u32 << rm_bit(q);
        }
    }
    t
}

/// Single-layout candidate state: 30 u32 (the AoSoA W=1 block).
#[derive(Clone, Copy)]
struct Mini {
    r: [[u32; 3]; 9],
    unsolved: [u32; 3],
}

enum Prop {
    Solved,
    Contradiction,
    Stuck,
}

impl Mini {
    fn from_cells(cells: &[Digit; CELLS], peer: &[[u32; 3]]) -> Self {
        let mut m = Mini { r: [[0; 3]; 9], unsolved: [0; 3] };
        for c in 0..CELLS {
            if cells[c] == 0 {
                let (l, bit) = (rm_lane(c), rm_bit(c));
                m.unsolved[l] |= 1 << bit;
                // candidate d iff no peer holds d.
                let mut used = 0u16;
                for &q in &PEERS[c] {
                    if cells[q] != 0 {
                        used |= 1 << (cells[q] - 1);
                    }
                }
                for d in 0..9 {
                    if used & (1 << d) == 0 {
                        m.r[d][l] |= 1 << bit;
                    }
                }
            }
        }
        let _ = peer;
        m
    }

    /// Restrict cell `c` to the candidate mask `keep` (9-bit).
    fn restrict(&mut self, c: usize, keep: u16) {
        let (l, bit) = (rm_lane(c), rm_bit(c));
        for d in 0..9 {
            if keep & (1 << d) == 0 {
                self.r[d][l] &= !(1 << bit);
            }
        }
    }

    /// Place digit d (0-based) at cell c: solve the cell, forbid d on peers.
    #[inline]
    fn place(&mut self, c: usize, d: usize, peer: &[[u32; 3]]) {
        let (l, bit) = (rm_lane(c), rm_bit(c));
        self.unsolved[l] &= !(1 << bit);
        for b in 0..3 {
            self.r[d][b] &= !peer[c][b];
        }
    }

    /// SMEAR peer union of `group` (one band-set): row-smear | box-expand | col-
    /// broadcast. Returns (peers, distinct_rows, distinct_cols, distinct_boxes).
    #[inline]
    fn smear(group: [u32; 3]) -> ([u32; 3], u32, u32, u32) {
        let fold = |g: u32| (g & 0x1FF) | ((g >> 9) & 0x1FF) | ((g >> 18) & 0x1FF);
        let col_occ = fold(group[0]) | fold(group[1]) | fold(group[2]);
        let colpeer = col_occ | (col_occ << 9) | (col_occ << 18);
        let mut out = [0u32; 3];
        let (mut rows, mut boxes) = (0u32, 0u32);
        for b in 0..3 {
            let g = group[b];
            let mut rp = 0u32;
            let mut bp = 0u32;
            for i in 0..3 {
                if (g >> (9 * i)) & 0x1FF != 0 {
                    rp |= 0x1FF << (9 * i);
                    rows |= 1 << (3 * b + i);
                }
            }
            for k in 0..3 {
                if g & BOX_CELLS[k] != 0 {
                    bp |= BOX_CELLS[k];
                    boxes |= 1 << (3 * b + k);
                }
            }
            out[b] = rp | bp | colpeer;
        }
        (out, rows.count_ones(), col_occ.count_ones(), boxes.count_ones())
    }

    /// Naked-single wave via smear place; false on contradiction.
    fn place_singles(&mut self, singles: [u32; 3]) -> bool {
        for d in 0..9 {
            let group = [singles[0] & self.r[d][0], singles[1] & self.r[d][1], singles[2] & self.r[d][2]];
            let n = group[0].count_ones() + group[1].count_ones() + group[2].count_ones();
            if n == 0 {
                continue;
            }
            let (peers, dr, dc, db) = Self::smear(group);
            if dr < n || dc < n || db < n {
                return false; // two same-digit singles share a unit
            }
            for b in 0..3 {
                self.unsolved[b] &= !group[b];
                self.r[d][b] &= !peers[b];
            }
        }
        true
    }

    /// Gather-free hidden singles in rows + boxes (row-major in-lane units), ALU
    /// exactly-one, NO column units, NO LC. Returns whether anything changed.
    fn hidden_singles(&mut self, peer: &[[u32; 3]]) -> bool {
        let mut changed = false;
        for b in 0..3 {
            for d in 0..9 {
                let mut live = self.r[d][b] & self.unsolved[b];
                // rows
                for rr in 0..3u32 {
                    let chunk = (live >> (9 * rr)) & 0x1FF;
                    if chunk != 0 && chunk & (chunk - 1) == 0 {
                        let cell = rm_cell(b, 9 * rr + chunk.trailing_zeros());
                        self.place(cell, d, peer);
                        changed = true;
                        live = self.r[d][b] & self.unsolved[b];
                    }
                }
                // boxes (gather each box's 9 bits)
                for k in 0..3u32 {
                    let bk = ((live >> (3 * k)) & 7)
                        | (((live >> (9 + 3 * k)) & 7) << 3)
                        | (((live >> (18 + 3 * k)) & 7) << 6);
                    if bk != 0 && bk & (bk - 1) == 0 {
                        let s = bk.trailing_zeros();
                        let bit = (s / 3) * 9 + 3 * k + s % 3;
                        let cell = rm_cell(b, bit);
                        self.place(cell, d, peer);
                        changed = true;
                        live = self.r[d][b] & self.unsolved[b];
                    }
                }
            }
        }
        changed
    }

    fn propagate(&mut self, peer: &[[u32; 3]]) -> Prop {
        loop {
            loop {
                let (mut ones, mut twos) = ([0u32; 3], [0u32; 3]);
                for d in 0..9 {
                    for b in 0..3 {
                        twos[b] |= ones[b] & self.r[d][b];
                        ones[b] |= self.r[d][b];
                    }
                }
                let mut dead = false;
                let mut solved = true;
                let mut singles = [0u32; 3];
                let mut has_single = false;
                for b in 0..3 {
                    if self.unsolved[b] & !ones[b] != 0 {
                        dead = true;
                    }
                    if self.unsolved[b] != 0 {
                        solved = false;
                    }
                    singles[b] = self.unsolved[b] & ones[b] & !twos[b];
                    if singles[b] != 0 {
                        has_single = true;
                    }
                }
                if dead {
                    return Prop::Contradiction;
                }
                if solved {
                    return Prop::Solved;
                }
                if !has_single {
                    break;
                }
                if !self.place_singles(singles) {
                    return Prop::Contradiction;
                }
            }
            if !self.hidden_singles(peer) {
                return Prop::Stuck;
            }
        }
    }

    /// Branch cell: a bivalue unsolved cell if any, else the first unsolved; with
    /// its candidate mask.
    fn branch_cell(&self) -> (usize, u16) {
        let (mut ones, mut twos, mut threes) = ([0u32; 3], [0u32; 3], [0u32; 3]);
        for d in 0..9 {
            for b in 0..3 {
                threes[b] |= twos[b] & self.r[d][b];
                twos[b] |= ones[b] & self.r[d][b];
                ones[b] |= self.r[d][b];
            }
        }
        let mut cell = usize::MAX;
        for b in 0..3 {
            let bivalue = self.unsolved[b] & twos[b] & !threes[b];
            let pick = if bivalue != 0 { bivalue } else { self.unsolved[b] };
            if pick != 0 {
                cell = rm_cell(b, pick.trailing_zeros());
                break;
            }
        }
        let (l, bit) = (rm_lane(cell), rm_bit(cell));
        let mut mask = 0u16;
        for d in 0..9 {
            if self.r[d][l] & (1 << bit) != 0 {
                mask |= 1 << d;
            }
        }
        (cell, mask)
    }

    /// Existence DFS, explicit stack (no recursion). Returns (found, nodes).
    fn exists(&self, peer: &[[u32; 3]]) -> (bool, u64) {
        let mut cur = *self;
        let mut stack: Vec<(Mini, usize, u16)> = Vec::new();
        let mut nodes = 0u64;
        loop {
            nodes += 1;
            match cur.propagate(peer) {
                Prop::Solved => return (true, nodes),
                Prop::Stuck => {
                    let (cell, mask) = cur.branch_cell();
                    let d = mask.trailing_zeros() as usize;
                    let rest = mask & (mask - 1);
                    stack.push((cur, cell, rest));
                    cur.place(cell, d, peer);
                }
                Prop::Contradiction => {
                    // backtrack to the nearest frame with a remaining candidate.
                    loop {
                        match stack.last_mut() {
                            None => return (false, nodes),
                            Some((saved, cell, rest)) => {
                                if *rest == 0 {
                                    stack.pop();
                                    continue;
                                }
                                let d = rest.trailing_zeros() as usize;
                                *rest &= *rest - 1;
                                cur = *saved;
                                cur.place(*cell, d, peer);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn main() {
    let mut attempts = 8000usize;
    let mut mode = 0u32;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
            "--mode" => mode = if it.next().as_deref() == Some("drill") { 1 } else { 0 },
            _ => {}
        }
    }
    let label = if mode == 0 { "train" } else { "drill" };
    let spec = spec_for_mode(mode);
    let baseline = spec.baseline_mask();
    let forced = spec.forced_mask();
    let peer = peer_table();
    let mut rng = Rng::from_seed(1);

    let mut queries = 0u64;
    let mut mismatches = 0u64;
    let mut mini_nodes = 0u64;
    let mut ref_nonunique = 0u64;

    for _ in 0..attempts {
        let solution = random_full_grid(&mut rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        let mut bb = BitBoard::from_board(&solution);
        let mut placed = Placed::from_board(&solution);
        let mut cells: [Digit; CELLS] = core::array::from_fn(|i| solution.cell(i));

        for i in positions {
            if cells[i] == 0 {
                continue;
            }
            let orig = cells[i];
            cells[i] = 0;
            let cand = bb.apply_clear(i, orig, &mut placed);
            let v_bit = digit_to_bit(orig);
            let alts = cand & !v_bit;
            if alts == 0 {
                continue;
            }

            // shipped dual-layout verdict
            let ref_verdict = bb.any_alt_solves(i, alts);

            // minimal single-layout explicit-stack verdict on the same board
            let mut mini = Mini::from_cells(&cells, &peer);
            mini.restrict(i, alts);
            let (mini_verdict, nodes) = mini.exists(&peer);

            queries += 1;
            mini_nodes += nodes;
            if ref_verdict {
                ref_nonunique += 1;
            }
            if mini_verdict != ref_verdict {
                mismatches += 1;
                if mismatches <= 3 {
                    eprintln!("MISMATCH at cell {i}: ref={ref_verdict} mini={mini_verdict}");
                }
            }

            // drive the real strip exactly as generator::attempt does
            if ref_verdict {
                cells[i] = orig;
                bb.apply_place(i, orig, &mut placed);
                continue;
            }
            let outcome = bb.baseline(baseline, forced);
            if !outcome.solved {
                cells[i] = orig;
                bb.apply_place(i, orig, &mut placed);
                continue;
            }
        }
    }

    println!("miniprober: {label}, {attempts} attempts\n");
    println!("  queries          {queries}");
    println!("  non-unique       {ref_nonunique}  ({:.1}%)", 100.0 * ref_nonunique as f64 / queries as f64);
    println!("  VERDICT MISMATCHES {mismatches}   <- MUST be 0 for the rewrite to be sound");
    println!("  mini nodes/query {:.3}  (vs shipped 2.48 full-toolbox; ~1.17x single-layout inflation expected)", mini_nodes as f64 / queries as f64);
}
