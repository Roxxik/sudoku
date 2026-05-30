use crate::board::Board;
use crate::techniques::{Deduction, REGISTRY, Step, TechniqueKind};

pub fn max_technique(trace: &[Step]) -> Option<TechniqueKind> {
    trace.iter().map(|s| s.technique).max_by_key(|t| t.difficulty())
}

pub fn deduction_counts(board: &Board) -> Vec<usize> {
    deduction_counts_filtered(board, |_| true)
}

pub fn deduction_counts_filtered<F: Fn(TechniqueKind) -> bool>(
    board: &Board,
    allow: F,
) -> Vec<usize> {
    let mut b = board.clone();
    let mut counts = Vec::new();
    loop {
        if b.is_solved() {
            return counts;
        }
        let count = distinct_conclusion_count_filtered(&b, &allow);
        if count == 0 {
            return counts;
        }
        counts.push(count);
        let step = next_step_filtered(&b, &allow).expect("count>0 means step exists");
        apply_step(&mut b, &step);
    }
}

fn distinct_conclusion_count_filtered<F: Fn(TechniqueKind) -> bool>(
    board: &Board,
    allow: F,
) -> usize {
    let steps = all_techniques_filtered(board, allow);
    let mut seen: Vec<crate::techniques::Deductions> = Vec::new();
    for s in &steps {
        if !seen.iter().any(|d| *d == s.deductions) {
            seen.push(s.deductions.clone());
        }
    }
    seen.len()
}

#[derive(Debug)]
pub struct SolveResult {
    pub board: Board,
    pub trace: Vec<Step>,
    pub solved: bool,
}

pub fn all_techniques(board: &Board) -> Vec<Step> {
    all_techniques_filtered(board, |_| true)
}

pub fn all_techniques_filtered<F: Fn(TechniqueKind) -> bool>(
    board: &Board,
    allow: F,
) -> Vec<Step> {
    let mut out = Vec::new();
    for def in REGISTRY {
        if !allow(def.kind) {
            continue;
        }
        out.extend((def.find_all)(board));
    }
    out
}

pub fn next_step_filtered(board: &Board, allow: impl Fn(TechniqueKind) -> bool) -> Option<Step> {
    for def in REGISTRY {
        if !allow(def.kind) {
            continue;
        }
        if let Some(s) = (def.find_first)(board) {
            return Some(s);
        }
    }
    None
}

pub fn next_step(board: &Board) -> Option<Step> {
    next_step_filtered(board, |_| true)
}

pub fn apply_step(board: &mut Board, step: &Step) {
    for d in &step.deductions {
        match *d {
            Deduction::Place { cell, digit } => board.place(cell, digit),
            Deduction::Eliminate { cell, digit } => {
                board.eliminate(cell, digit);
            }
        }
    }
}

pub fn solve_solvable_only(mut board: Board) -> bool {
    loop {
        if board.is_solved() {
            return true;
        }
        match next_step(&board) {
            Some(step) => apply_step(&mut board, &step),
            None => return false,
        }
    }
}

pub fn solve(board: Board) -> SolveResult {
    solve_filtered(board, |_| true)
}

pub fn solve_filtered<F: Fn(TechniqueKind) -> bool>(mut board: Board, allow: F) -> SolveResult {
    let mut trace = Vec::new();
    loop {
        if board.is_solved() {
            return SolveResult {
                board,
                trace,
                solved: true,
            };
        }
        match next_step_filtered(&board, &allow) {
            Some(step) => {
                apply_step(&mut board, &step);
                trace.push(step);
            }
            None => {
                return SolveResult {
                    board,
                    trace,
                    solved: false,
                };
            }
        }
    }
}
