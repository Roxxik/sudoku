use crate::board::Board;
use crate::techniques::{Deduction, REGISTRY, Step, TechniqueKind};

pub fn max_technique(trace: &[Step]) -> Option<TechniqueKind> {
    trace.iter().map(|s| s.technique).max_by_key(|t| t.difficulty())
}

pub fn deduction_counts(board: &Board) -> Vec<usize> {
    let mut b = board.clone();
    let mut counts = Vec::new();
    loop {
        if b.is_solved() {
            return counts;
        }
        let count = distinct_conclusion_count(&b);
        if count == 0 {
            return counts;
        }
        counts.push(count);
        let step = next_step(&b).expect("count>0 means step exists");
        apply_step(&mut b, &step);
    }
}

fn distinct_conclusion_count(board: &Board) -> usize {
    let steps = all_techniques(board);
    let mut seen: Vec<Vec<Deduction>> = Vec::new();
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
    let mut out = Vec::new();
    for def in REGISTRY {
        out.extend((def.find_all)(board));
    }
    out
}

pub fn next_step_filtered(board: &Board, allow: impl Fn(TechniqueKind) -> bool) -> Option<Step> {
    for def in REGISTRY {
        if !allow(def.kind) {
            continue;
        }
        if let Some(s) = (def.find_all)(board).into_iter().next() {
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

pub fn solve(mut board: Board) -> SolveResult {
    let mut trace = Vec::new();
    loop {
        if board.is_solved() {
            return SolveResult {
                board,
                trace,
                solved: true,
            };
        }
        match next_step(&board) {
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
