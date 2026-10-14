// Solver probe for the move-log analysis (scripts/movelog-analyze,
// docs/grader-human-movelogs.md). The script replays every uploaded move log into
// board states; this answers, per state, what the solver sees there.
//
// stdin, one line per moment:  <state> \t <masks> \t <target>
//   state   81 chars, '.' = empty -- the placed digits at the start of the interval
//   masks   81 comma-separated 9-bit candidate masks built exactly like play.js
//           solverBoard (0 = unspecified, keep grid-derived), or '-' for none
//   target  the cell the player placed next (0..80)
//
// stdout, one TSV line per input line:
//   gate_marks  hardest technique on the easiest curriculum path to the first
//               placement, reasoning over the player's notes (what the hint engine sees)
//   gate_grid   the same from placed digits alone (notes ignored)
//   difficulty  core's TechniqueKind::difficulty of gate_marks (0 if none)
//   singles     cells placeable by a single on the grid board right now
//   ns hs       of those, how many by naked / by hidden single
//   t_ns t_hs   whether the target cell is a naked / hidden single (0/1)
//
// The gates walk only the 16 curriculum techniques: core also ships turbot and
// finned fish, which would otherwise win where they happen to come first in
// REGISTRY order although no generated puzzle needs them.
//
// Usage: cargo run --release -p sudoku-core --example movelog_probe < moments.tsv

use std::collections::HashSet;
use std::io::{BufRead, BufWriter, Write};

use sudoku_core::board::Board;
use sudoku_core::solver::{all_techniques_filtered, apply_step, next_step_filtered};
use sudoku_core::techniques::{Deduction, TechniqueKind};

fn curriculum(k: TechniqueKind) -> bool {
    use TechniqueKind::*;
    matches!(
        k,
        NakedSingle
            | HiddenSingle
            | LockedCandidatesPointing
            | LockedCandidatesClaiming
            | NakedPair
            | HiddenPair
            | NakedTriple
            | HiddenTriple
            | NakedQuad
            | HiddenQuad
            | XWing
            | Swordfish
            | Jellyfish
            | XYWing
            | XYZWing
            | WWing
    )
}

// Hardest technique (by difficulty) on the easiest-step path until the first placement.
fn gate(mut b: Board) -> Option<TechniqueKind> {
    let mut max: Option<TechniqueKind> = None;
    while !b.is_solved() {
        let step = next_step_filtered(&b, curriculum)?;
        if max.is_none_or(|m| step.technique.difficulty() > m.difficulty()) {
            max = Some(step.technique);
        }
        let places = step.deductions.iter().any(|d| matches!(d, Deduction::Place { .. }));
        apply_step(&mut b, &step);
        if places {
            return max;
        }
    }
    max
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = BufWriter::new(std::io::stdout());
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        if line.is_empty() {
            continue;
        }
        let mut it = line.split('\t');
        let (Some(state), Some(masks), Some(target)) = (it.next(), it.next(), it.next()) else {
            panic!("bad input line: {line}");
        };
        let target: usize = target.parse().expect("target cell");
        let grid = Board::parse(state).expect("state");

        // The marks-aware board, built like the wasm Board constructor: every grid-derived
        // candidate a non-zero mask omits is eliminated (a mask can never add one).
        let mut marked = grid.clone();
        if masks != "-" {
            for (i, m) in masks.split(',').enumerate() {
                let m: u16 = m.parse().expect("mask");
                if m == 0 || !marked.is_empty(i) {
                    continue;
                }
                for d in 1..=9u8 {
                    if marked.candidates(i) & (1 << (d - 1)) != 0 && m & (1 << (d - 1)) == 0 {
                        marked.eliminate(i, d);
                    }
                }
            }
        }

        let (mut any, mut ns, mut hs) = (HashSet::new(), HashSet::new(), HashSet::new());
        let singles = all_techniques_filtered(&grid, |k| {
            matches!(k, TechniqueKind::NakedSingle | TechniqueKind::HiddenSingle)
        });
        for s in &singles {
            for d in &s.deductions {
                if let Deduction::Place { cell, .. } = *d {
                    any.insert(cell);
                    if s.technique == TechniqueKind::NakedSingle {
                        ns.insert(cell);
                    } else {
                        hs.insert(cell);
                    }
                }
            }
        }

        let name = |t: Option<TechniqueKind>| t.map(|t| t.cli_name()).unwrap_or("none");
        let gm = gate(marked);
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            name(gm),
            name(gate(grid)),
            gm.map(|t| t.difficulty()).unwrap_or(0),
            any.len(),
            ns.len(),
            hs.len(),
            ns.contains(&target) as u8,
            hs.contains(&target) as u8,
        )
        .expect("stdout");
    }
}
