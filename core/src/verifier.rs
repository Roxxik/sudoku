use crate::board::Board;
use crate::solver::{apply_step, next_step_filtered};
use crate::spec::{Spec, TechniqueSet, Usage};
use crate::techniques::TechniqueKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// The baseline (`Allowed | Forced`) solver got stuck. Reason isn't
    /// diagnosed — could be that an out-of-spec technique is required,
    /// a `Conceded` technique would have been required, or the puzzle isn't
    /// technique-solvable at all. The "stuck" signal is what we act on; if
    /// you want a reason, run the canonical solver yourself.
    BaselineCannotSolve,
    /// A `Forced { count }` technique didn't reach its required count of uses
    /// in the avoid-target walk — i.e., `actual` fell short of `required`.
    ForcedTechniqueShort {
        technique: TechniqueKind,
        required: usize,
        actual: usize,
    },
    /// A `RequireAny` constraint didn't reach its required count of uses
    /// drawn from `kinds` — `actual` fell short of `required`.
    ForcedAnyShort {
        kinds: TechniqueSet,
        required: usize,
        actual: usize,
    },
}

pub fn verify(board: &Board, spec: &Spec) -> Result<(), Vec<Violation>> {
    let mut violations = Vec::new();

    // Positive check: baseline (Allowed | Forced) alone must solve.
    if !solvable_with(board, |t| spec.is_baseline(t)) {
        violations.push(Violation::BaselineCannotSolve);
    }

    // Forcing check: each Forced technique must be unavoidable. The avoid-T
    // walk has access to Allowed and Conceded (everything in scope but T) —
    // so for the check to pass, T must beat the full sub-T toolbox the spec
    // grants the hypothetical solver.
    for (t, usage) in spec.iter_usages() {
        if let Usage::Forced { count } = usage {
            let min_uses = min_required_uses_of(board, spec, |k| k == t);
            if min_uses < count.get() {
                violations.push(Violation::ForcedTechniqueShort {
                    technique: t,
                    required: count.get(),
                    actual: min_uses,
                });
            }
        }
    }

    for require in &spec.require_any {
        let min_uses = min_required_uses_of(board, spec, |k| require.kinds.contains(k));
        if min_uses < require.count.get() {
            violations.push(Violation::ForcedAnyShort {
                kinds: require.kinds,
                required: require.count.get(),
                actual: min_uses,
            });
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

/// Walk a trace that avoids `is_target` whenever possible, using only
/// techniques in `spec` (Allowed + Forced + Conceded — the avoid-walk
/// notably *includes* Conceded, so a hypothetical solver gets the whole
/// concession toolbox to try to dodge the target). Returns how many times
/// a target step had to be applied — a tight approximation of "minimum
/// target-uses across all valid traces" given the soundness of deductions
/// in this solver.
fn min_required_uses_of(
    board: &Board,
    spec: &Spec,
    is_target: impl Fn(TechniqueKind) -> bool,
) -> usize {
    let mut b = board.clone();
    let mut count = 0;
    loop {
        if b.is_solved() {
            return count;
        }
        if let Some(s) =
            next_step_filtered(&b, |t| spec.is_in_scope(t) && !is_target(t))
        {
            apply_step(&mut b, &s);
            continue;
        }
        match next_step_filtered(&b, |t| spec.is_in_scope(t)) {
            None => return count,
            Some(s) => {
                if is_target(s.technique) {
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
    use crate::techniques::Family;

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
            .any(|v| matches!(v, Violation::BaselineCannotSolve)));
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
    fn empty_board_is_rejected_under_allow_all() {
        // A puzzle so empty that our techniques cannot make progress. Under
        // any spec the baseline gets stuck immediately.
        let board = crate::Board::parse(&".".repeat(81)).unwrap();
        let spec = Spec::allow_all();
        let r = verify(&board, &spec);
        assert!(matches!(
            r,
            Err(ref v) if v.iter().any(|x| matches!(x, Violation::BaselineCannotSolve))
        ));
    }

    #[test]
    fn require_family_short_reports_forced_any() {
        // Norvig easy solves with singles only — no fish ever fires, so a
        // `require_family(Fish, 1)` must surface a ForcedAnyShort violation.
        let board = crate::Board::parse(
            "003020600900305001001806400008102900700000008006708200002609500800203009005010300",
        )
        .unwrap();
        let spec = Spec::allow_all().require_family(Family::Fish, 1);
        let r = verify(&board, &spec);
        assert!(
            matches!(
                r,
                Err(ref v) if v.iter().any(|x| matches!(x, Violation::ForcedAnyShort { required: 1, actual: 0, .. }))
            ),
            "expected ForcedAnyShort for Fish family, got {:?}",
            r,
        );
    }
}
