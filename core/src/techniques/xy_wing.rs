use smallvec::smallvec;

use crate::board::{Board, CELLS, PEERS, iter_digits, popcount};
use crate::techniques::{Deduction, Step, TechniqueKind};
use crate::util::for_each_combination;

pub fn find_xyz_wing(board: &Board) -> Vec<Step> {
    let mut out = Vec::new();
    find_xyz_wing_each(board, |s| {
        out.push(s);
        true
    });
    out
}

pub fn find_first_xyz_wing(board: &Board) -> Option<Step> {
    let mut found = None;
    find_xyz_wing_each(board, |s| {
        found = Some(s);
        false
    });
    found
}

fn find_xyz_wing_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    let trivalues: Vec<(usize, u16)> = (0..CELLS)
        .filter(|&i| board.is_empty(i) && popcount(board.candidates(i)) == 3)
        .map(|i| (i, board.candidates(i)))
        .collect();
    let bivalue_cells: Vec<(usize, u16)> = (0..CELLS)
        .filter(|&i| board.is_empty(i) && popcount(board.candidates(i)) == 2)
        .map(|i| (i, board.candidates(i)))
        .collect();

    for &(pivot, pcands) in &trivalues {
        let peers_of_pivot: &[usize; 20] = &PEERS[pivot];
        let wings: Vec<(usize, u16)> = bivalue_cells
            .iter()
            .copied()
            .filter(|&(c, cands)| peers_of_pivot.contains(&c) && cands & !pcands == 0)
            .collect();
        if wings.len() < 2 {
            continue;
        }
        let mut keep_going = true;
        for_each_combination(&wings, 2, |combo| {
            let (a, acands) = combo[0];
            let (b, bcands) = combo[1];
            let shared = acands & bcands;
            if popcount(shared) != 1 {
                return true;
            }
            // The two wings together must cover the pivot's 3 candidates.
            if (acands | bcands) != pcands {
                return true;
            }
            let z = iter_digits(shared).next().unwrap();
            let z_candidates = shared;
            let mut eliminations = Vec::new();
            for c in 0..CELLS {
                if c == pivot || c == a || c == b {
                    continue;
                }
                if !board.is_empty(c) {
                    continue;
                }
                if board.candidates(c) & z_candidates == 0 {
                    continue;
                }
                if !peers_of_pivot.contains(&c)
                    || !PEERS[a].contains(&c)
                    || !PEERS[b].contains(&c)
                {
                    continue;
                }
                eliminations.push(Deduction::Eliminate { cell: c, digit: z });
            }
            if eliminations.is_empty() {
                return true;
            }
            let cont = emit(Step {
                technique: TechniqueKind::XYZWing,
                deductions: eliminations.into(),
                focus_cells: smallvec![pivot, a, b],
                focus_house: None,
            });
            if !cont {
                keep_going = false;
            }
            cont
        });
        if !keep_going {
            return;
        }
    }
}

pub fn find_all(board: &Board) -> Vec<Step> {
    let mut out = Vec::new();
    find_each(board, |s| {
        out.push(s);
        true
    });
    out
}

pub fn find_first(board: &Board) -> Option<Step> {
    let mut found = None;
    find_each(board, |s| {
        found = Some(s);
        false
    });
    found
}

fn find_each<F: FnMut(Step) -> bool>(board: &Board, mut emit: F) {
    let bivalues: Vec<(usize, u16)> = (0..CELLS)
        .filter(|&i| board.is_empty(i) && popcount(board.candidates(i)) == 2)
        .map(|i| (i, board.candidates(i)))
        .collect();

    for &(pivot, pcands) in &bivalues {
        let peers_of_pivot: &[usize; 20] = &PEERS[pivot];
        for (ai, &(a, acands)) in bivalues.iter().enumerate() {
            if a == pivot || !peers_of_pivot.contains(&a) {
                continue;
            }
            let shared_with_p = pcands & acands;
            if popcount(shared_with_p) != 1 {
                continue;
            }
            let z_candidates = acands & !shared_with_p;
            if z_candidates & pcands != 0 {
                continue;
            }
            let other_pivot_digit = pcands & !shared_with_p;
            let required_b = other_pivot_digit | z_candidates;

            for &(b, bcands) in bivalues.iter().skip(ai + 1) {
                if b == pivot || b == a {
                    continue;
                }
                if !peers_of_pivot.contains(&b) {
                    continue;
                }
                if bcands != required_b {
                    continue;
                }
                let z = iter_digits(z_candidates).next().unwrap();
                let mut eliminations = Vec::new();
                for c in 0..CELLS {
                    if c == pivot || c == a || c == b {
                        continue;
                    }
                    if !board.is_empty(c) {
                        continue;
                    }
                    if board.candidates(c) & z_candidates == 0 {
                        continue;
                    }
                    if !PEERS[a].contains(&c) || !PEERS[b].contains(&c) {
                        continue;
                    }
                    eliminations.push(Deduction::Eliminate { cell: c, digit: z });
                }
                if eliminations.is_empty() {
                    continue;
                }
                let cont = emit(Step {
                    technique: TechniqueKind::XYWing,
                    deductions: eliminations.into(),
                    focus_cells: smallvec![pivot, a, b],
                    focus_house: None,
                });
                if !cont {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_basic_xy_wing() {
        // Construct a small XY-Wing:
        // Pivot R1C1 = {1,2}, wing A R1C4 = {1,3} (same row), wing B R4C1 = {2,3} (same col)
        // Z = 3 should be eliminated from cells that see both R1C4 and R4C1:
        // that's R4C4 specifically (and other intersections of row 4 with col 4 — R4C4 is the unique one).
        let mut b = Board::empty();
        let pivot = 0; // R1C1
        let wa = 3;    // R1C4
        let wb = 27;   // R4C1
        let target = 30; // R4C4

        // Pivot = {1,2}
        for d in [3u8, 4, 5, 6, 7, 8, 9] {
            b.eliminate(pivot, d);
        }
        // wing A = {1,3}
        for d in [2u8, 4, 5, 6, 7, 8, 9] {
            b.eliminate(wa, d);
        }
        // wing B = {2,3}
        for d in [1u8, 4, 5, 6, 7, 8, 9] {
            b.eliminate(wb, d);
        }
        // We need target to still have 3 as a candidate (which it does on an empty board).
        let steps = find_all(&b);
        let step = steps
            .iter()
            .find(|s| s.focus_cells[..] == [pivot, wa, wb] || s.focus_cells[..] == [pivot, wb, wa])
            .expect("expected an XY-Wing");
        assert!(
            step.deductions.iter().any(
                |d| matches!(d, Deduction::Eliminate { cell, digit: 3 } if *cell == target)
            ),
            "expected elimination of 3 from R4C4, got {:?}",
            step.deductions,
        );
    }
}
