use std::collections::HashMap;

use crate::board::Board;
use crate::solver::{apply_step, next_step_filtered, solve};
use crate::spec::{Spec, Usage};
use crate::techniques::TechniqueKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    ForbiddenTechniqueAppeared {
        technique: TechniqueKind,
        count: usize,
    },
    ForcedTechniqueShort {
        technique: TechniqueKind,
        required: usize,
        actual: usize,
    },
    UnsolvableWithImplementedTechniques,
}

pub fn verify(board: &Board, spec: &Spec) -> Result<(), Vec<Violation>> {
    let mut violations = Vec::new();
    let allowed = |t: TechniqueKind| spec.usages.contains_key(&t);

    // Can we solve using only the techniques the spec allows? If yes, no
    // forbidden technique is required. If no, find what the puzzle would have
    // needed and report it.
    if !solvable_with(board, allowed) {
        let unrestricted = solve(board.clone());
        if !unrestricted.solved {
            violations.push(Violation::UnsolvableWithImplementedTechniques);
        } else {
            let mut by_tech: HashMap<TechniqueKind, usize> = HashMap::new();
            for s in &unrestricted.trace {
                if !allowed(s.technique) {
                    *by_tech.entry(s.technique).or_insert(0) += 1;
                }
            }
            for (technique, count) in by_tech {
                violations.push(Violation::ForbiddenTechniqueAppeared { technique, count });
            }
        }
    }

    for (&t, usage) in &spec.usages {
        if let Usage::Forced { count } = *usage {
            let min_uses = min_required_uses(board, spec, t);
            if min_uses < count.get() {
                violations.push(Violation::ForcedTechniqueShort {
                    technique: t,
                    required: count.get(),
                    actual: min_uses,
                });
            }
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

fn solvable_with(board: &Board, allowed: impl Fn(TechniqueKind) -> bool) -> bool {
    let mut b = board.clone();
    loop {
        if b.is_solved() {
            return true;
        }
        match next_step_filtered(&b, &allowed) {
            None => return false,
            Some(s) => apply_step(&mut b, &s),
        }
    }
}

/// Walk a trace that avoids `target` whenever possible, using only techniques
/// in `spec`. Returns how many times `target` had to be applied — a tight
/// approximation of "minimum t-uses across all valid traces" given soundness
/// of deductions in this solver.
fn min_required_uses(board: &Board, spec: &Spec, target: TechniqueKind) -> usize {
    let mut b = board.clone();
    let mut count = 0;
    loop {
        if b.is_solved() {
            return count;
        }
        if let Some(s) = next_step_filtered(&b, |t| spec.usages.contains_key(&t) && t != target) {
            apply_step(&mut b, &s);
            continue;
        }
        match next_step_filtered(&b, |t| spec.usages.contains_key(&t)) {
            None => return count,
            Some(s) => {
                if s.technique == target {
                    count += 1;
                }
                apply_step(&mut b, &s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rng;
    use crate::generator::make_puzzle;

    #[test]
    fn easy_puzzle_satisfies_singles_only_spec() {
        let mut rng = Rng::from_seed(42);
        let p = make_puzzle(&mut rng, true);
        // Most random puzzles solve with singles only (we measured this).
        // We want a puzzle that we know solves with singles only. Use Norvig's.
        let board = crate::Board::parse(
            "003020600900305001001806400008102900700000008006708200002609500800203009005010300",
        )
        .unwrap();
        let spec = Spec::allow_up_to(TechniqueKind::HiddenSingle);
        let r = verify(&board, &spec);
        assert!(r.is_ok(), "expected Ok, got {:?}", r);
        let _ = p;
    }

    #[test]
    fn allow_all_accepts_anything_solvable() {
        let mut rng = Rng::from_seed(7);
        let p = make_puzzle(&mut rng, true);
        let spec = Spec::allow_all();
        assert!(verify(&p.puzzle, &spec).is_ok());
    }

    #[test]
    fn empty_spec_rejects_any_puzzle_with_deductions() {
        let board = crate::Board::parse(
            "003020600900305001001806400008102900700000008006708200002609500800203009005010300",
        )
        .unwrap();
        let spec = Spec::empty();
        let r = verify(&board, &spec);
        assert!(r.is_err());
        let violations = r.unwrap_err();
        assert!(violations
            .iter()
            .any(|v| matches!(v, Violation::ForbiddenTechniqueAppeared { .. })));
    }

    #[test]
    fn singles_only_spec_rejects_puzzles_needing_pairs() {
        use crate::generator::make_puzzle_needing;
        let mut rng = Rng::from_seed(123);
        let fr = make_puzzle_needing(&mut rng, TechniqueKind::HiddenPair, 5000)
            .expect("should find a hidden-pair puzzle within 5000 attempts");
        let spec = Spec::allow_up_to(TechniqueKind::HiddenSingle);
        let r = verify(&fr.puzzle.puzzle, &spec);
        assert!(r.is_err(), "expected violation for hidden-pair appearing under singles-only spec");
    }

    #[test]
    fn forced_shortfall_reports_violation() {
        let board = crate::Board::parse(
            "003020600900305001001806400008102900700000008006708200002609500800203009005010300",
        )
        .unwrap();
        let spec = Spec::allow_up_to(TechniqueKind::HiddenSingle)
            .require(TechniqueKind::HiddenTriple, 1);
        let r = verify(&board, &spec);
        assert!(matches!(
            r,
            Err(ref v) if v.iter().any(|x| matches!(x, Violation::ForcedTechniqueShort { technique: TechniqueKind::HiddenTriple, .. }))
        ));
    }

    #[test]
    fn unsolvable_with_techniques_is_reported() {
        // A puzzle so empty that our techniques cannot make progress.
        // Use a single-cell-given puzzle.
        let mut s = String::new();
        for _ in 0..81 {
            s.push('.');
        }
        let board = crate::Board::parse(&s).unwrap();
        let spec = Spec::allow_all();
        let r = verify(&board, &spec);
        assert!(matches!(
            r,
            Err(ref v) if v.iter().any(|x| matches!(x, Violation::UnsolvableWithImplementedTechniques))
        ));
    }
}
