use crate::board::{BOX_UNITS, Board, CELLS, COL_UNITS, PEERS, ROW_UNITS};
use crate::generator::random_full_grid;
use crate::rng::Rng;
use crate::solver::{apply_step, next_step_filtered};
use crate::uniqueness;
use crate::spec::Spec;
use crate::techniques::TechniqueKind;
use crate::verifier::verify;

pub trait Constructor {
    fn try_extend(&self, solution: &Board, current: &Board, rng: &mut Rng) -> Option<Board>;
}

const SHUFFLES_PER_TEMPLATE: usize = 8;

pub struct HiddenTripleConstructor {
    pub spec: Spec,
}

impl HiddenTripleConstructor {
    pub fn for_spec(spec: Spec) -> Self {
        Self { spec }
    }
}

fn peers_holding_digit(board: &Board, cell: usize, digit: u8) -> Vec<usize> {
    PEERS[cell]
        .iter()
        .copied()
        .filter(|&p| board.cell(p) == digit)
        .collect()
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

impl Constructor for HiddenTripleConstructor {
    fn try_extend(&self, solution: &Board, _current: &Board, rng: &mut Rng) -> Option<Board> {
        let target = TechniqueKind::HiddenTriple;
        let allowed = |t: TechniqueKind| self.spec.usages.contains_key(&t);
        let allowed_without_target = |t: TechniqueKind| allowed(t) && t != target;

        // Step 1: pick the geometry — unit U, three "triple" cells {x,y,z},
        // and an extra empty q in U.
        let unit: [usize; 9] = match rng.range(3) {
            0 => ROW_UNITS[rng.range(9)],
            1 => COL_UNITS[rng.range(9)],
            _ => BOX_UNITS[rng.range(9)],
        };
        let mut idx_order: Vec<usize> = (0..9).collect();
        rng.shuffle(&mut idx_order);
        let triple_cells: [usize; 3] = [
            unit[idx_order[0]],
            unit[idx_order[1]],
            unit[idx_order[2]],
        ];
        let q = unit[idx_order[3]];
        let triple_digits: [u8; 3] = [
            solution.cell(triple_cells[0]),
            solution.cell(triple_cells[1]),
            solution.cell(triple_cells[2]),
        ];
        let d4 = solution.cell(q);

        let mut extras_order: Vec<usize> = (0..3).collect();
        rng.shuffle(&mut extras_order);

        for ei in extras_order {
            let extras_cell = triple_cells[ei];

            // Step 2: keep d4 as a candidate at extras_cell so the triple is
            // hidden, not naked. Every peer of extras_cell that holds d4 in
            // the solution (and lies outside U) must be empty in the puzzle.
            let must_be_empty: Vec<usize> = peers_holding_digit(solution, extras_cell, d4)
                .into_iter()
                .filter(|c| !unit.contains(c))
                .collect();

            // Step 3: for each triple digit d, pin some peer-of-q outside U
            // (and not in must_be_empty) as a given so d is eliminated from
            // q's candidates.
            let mut must_be_given: Vec<usize> = Vec::with_capacity(3);
            let mut step3_ok = true;
            for &d in &triple_digits {
                let mut candidates: Vec<usize> = peers_holding_digit(solution, q, d)
                    .into_iter()
                    .filter(|c| !unit.contains(c) && !must_be_empty.contains(c))
                    .collect();
                if candidates.is_empty() {
                    step3_ok = false;
                    break;
                }
                rng.shuffle(&mut candidates);
                must_be_given.push(candidates[0]);
            }
            if !step3_ok {
                continue;
            }

            // Build the starting board.
            let mut board = solution.clone();
            for &c in &triple_cells {
                board.clear(c);
            }
            board.clear(q);
            for &c in &must_be_empty {
                board.clear(c);
            }

            // Strippable cells: not pinned (must_be_given) and not in U.
            // U cells outside the template stay as givens to preserve the
            // "exactly 4 empties in U" invariant.
            let strippable_template: Vec<usize> = (0..CELLS)
                .filter(|i| !board.is_empty(*i))
                .filter(|i| !must_be_given.contains(i))
                .filter(|i| !unit.contains(i))
                .collect();

            let initial_board = board.clone();
            for _ in 0..SHUFFLES_PER_TEMPLATE {
                let mut strippable = strippable_template.clone();
                rng.shuffle(&mut strippable);
                let mut board = initial_board.clone();

                for i in strippable {
                    let mut candidate = board.clone();
                    candidate.clear(i);
                    if uniqueness::count_solutions(&candidate, 2) != 1 {
                        continue;
                    }
                    // Fast path: walk without target. If it solves, target
                    // isn't required — commit the strip and continue.
                    if solvable_with(&candidate, allowed_without_target) {
                        board = candidate;
                        continue;
                    }
                    // Without-target walk got stuck. Confirm the candidate is
                    // still allowed-solvable, then we have a Forced(target) hit.
                    if solvable_with(&candidate, allowed) {
                        return Some(candidate);
                    }
                    // Not solvable at all — skip this strip.
                }
            }
        }
        None
    }
}

pub fn construct_with(
    constructor: &impl Constructor,
    rng: &mut Rng,
    max_attempts: usize,
) -> Option<(Board, usize)> {
    for attempt in 1..=max_attempts {
        let solution = random_full_grid(rng);
        if let Some(b) = constructor.try_extend(&solution, &solution, rng) {
            return Some((b, attempt));
        }
    }
    None
}

pub fn compose(spec: &Spec, rng: &mut Rng) -> Option<Board> {
    if let Some(crate::spec::Usage::Forced { .. }) = spec.usages.get(&TechniqueKind::HiddenTriple)
    {
        let constructor = HiddenTripleConstructor::for_spec(spec.clone());
        for _ in 0..500 {
            let solution = random_full_grid(rng);
            if let Some(b) = constructor.try_extend(&solution, &solution, rng) {
                if verify(&b, spec).is_ok() {
                    return Some(b);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn narrow_training_spec() -> Spec {
        Spec::allow_up_to(TechniqueKind::HiddenSingle).require(TechniqueKind::HiddenTriple, 1)
    }

    #[test]
    fn hidden_triple_constructor_under_narrow_spec() {
        let spec = narrow_training_spec();
        let constructor = HiddenTripleConstructor::for_spec(spec.clone());
        for seed in [7u64, 42, 123, 2024, 1] {
            let mut rng = Rng::from_seed(seed);
            let (board, _attempts) = construct_with(&constructor, &mut rng, 100)
                .unwrap_or_else(|| panic!("seed {}: constructor should succeed", seed));
            assert!(
                verify(&board, &spec).is_ok(),
                "seed {}: produced board fails the narrow training spec",
                seed,
            );
        }
    }
}
