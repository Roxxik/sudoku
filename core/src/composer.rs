use crate::board::{BOX_UNITS, Board, CELLS, COL_UNITS, PEERS, ROW_UNITS, UnitKind};
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

/// The geometry the hidden-subset constructor seeded for a successful build:
/// the unit it confined the subset to, the subset cells, and the extra cell q.
/// Returned by [`HiddenSubsetConstructor::try_extend_traced`] so callers can
/// check whether the *forced* subset in the finished puzzle is actually the
/// one that was seeded (vs. a different subset that emerged during stripping).
#[derive(Debug, Clone)]
pub struct SeedGeometry {
    pub unit_kind: UnitKind,
    pub unit_index: usize,
    pub subset_cells: Vec<usize>,
    pub q: usize,
}

/// Essence-seeded constructor for the hidden-subset family (hidden triple,
/// hidden quad). Both share one geometry: `size` digits confined to `size`
/// cells of a unit, with at least one of those cells carrying a non-subset
/// candidate (so the subset is *hidden*, not naked, and there is an
/// elimination to make). `size` is the only thing that changes between triple
/// and quad — the invariant, the pinning, and the seed-gated strip-down are
/// identical. See [`construct_hidden_subset`].
pub struct HiddenSubsetConstructor {
    pub spec: Spec,
    pub size: usize,
    pub target: TechniqueKind,
}

impl HiddenSubsetConstructor {
    pub fn triple(spec: Spec) -> Self {
        Self { spec, size: 3, target: TechniqueKind::HiddenTriple }
    }

    pub fn quad(spec: Spec) -> Self {
        Self { spec, size: 4, target: TechniqueKind::HiddenQuad }
    }
}

impl Constructor for HiddenSubsetConstructor {
    fn try_extend(&self, solution: &Board, _current: &Board, rng: &mut Rng) -> Option<Board> {
        self.try_extend_traced(solution, rng).map(|(b, _)| b)
    }
}

impl HiddenSubsetConstructor {
    /// As [`Constructor::try_extend`], but also returns the seeded
    /// [`SeedGeometry`] — for diagnostics that check seed-vs-forced fidelity.
    pub fn try_extend_traced(
        &self,
        solution: &Board,
        rng: &mut Rng,
    ) -> Option<(Board, SeedGeometry)> {
        construct_hidden_subset(&self.spec, self.size, self.target, solution, rng)
    }
}

/// Back-compat alias: the original triple-only constructor. Delegates to the
/// generalized [`HiddenSubsetConstructor`] with `size = 3`.
pub struct HiddenTripleConstructor {
    inner: HiddenSubsetConstructor,
}

impl HiddenTripleConstructor {
    pub fn for_spec(spec: Spec) -> Self {
        Self { inner: HiddenSubsetConstructor::triple(spec) }
    }
}

impl Constructor for HiddenTripleConstructor {
    fn try_extend(&self, solution: &Board, current: &Board, rng: &mut Rng) -> Option<Board> {
        self.inner.try_extend(solution, current, rng)
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

/// Build a puzzle whose forced hidden subset is *the seeded one* — generalizing
/// from the original triple constructor (`size = 3`) to quads (`size = 4`).
///
/// Seed the geometry, then strip to a forced-subset puzzle.
///
/// NOTE (measured): this is NOT faithful — the forced subset is essentially
/// never the seeded one. The seed is dissolved / dodged by the strip-down and
/// the bottleneck forms elsewhere (0% seed-vs-forced fidelity across exact-cell
/// and unit-level matching; gating the strip to drive the stall onto the seed
/// builds 0/200k). The forced technique's location is an emergent global
/// property of the minimal grid and cannot be placed by seeding. See the
/// `seed_fidelity` example. Kept as the working prototype: it produces *some*
/// forced subset, just not the seeded one, so it offers no efficiency over the
/// random-restart generator.
fn construct_hidden_subset(
    spec: &Spec,
    size: usize,
    target: TechniqueKind,
    solution: &Board,
    rng: &mut Rng,
) -> Option<(Board, SeedGeometry)> {
    let allowed = |t: TechniqueKind| spec.is_in_scope(t);
    let allowed_without_target = |t: TechniqueKind| allowed(t) && t != target;
    // Step 1: pick the geometry — unit U, `size` "subset" cells, and an extra
    // empty q in U. (For a triple that's 3 cells; for a quad, 4.)
    let (unit_kind, unit_index) = match rng.range(3) {
        0 => (UnitKind::Row, rng.range(9)),
        1 => (UnitKind::Col, rng.range(9)),
        _ => (UnitKind::Box, rng.range(9)),
    };
    let unit: [usize; 9] = match unit_kind {
        UnitKind::Row => ROW_UNITS[unit_index],
        UnitKind::Col => COL_UNITS[unit_index],
        UnitKind::Box => BOX_UNITS[unit_index],
    };
    let mut idx_order: Vec<usize> = (0..9).collect();
    rng.shuffle(&mut idx_order);
    let subset_cells: Vec<usize> = (0..size).map(|k| unit[idx_order[k]]).collect();
    let q = unit[idx_order[size]];
    let subset_digits: Vec<u8> = subset_cells.iter().map(|&c| solution.cell(c)).collect();
    let d4 = solution.cell(q);

    let mut extras_order: Vec<usize> = (0..size).collect();
    rng.shuffle(&mut extras_order);

    for ei in extras_order {
        let extras_cell = subset_cells[ei];

        // Step 2: keep d4 as a candidate at extras_cell so the subset is
        // hidden, not naked. Every peer of extras_cell that holds d4 in
        // the solution (and lies outside U) must be empty in the puzzle.
        let must_be_empty: Vec<usize> = peers_holding_digit(solution, extras_cell, d4)
            .into_iter()
            .filter(|c| !unit.contains(c))
            .collect();

        // Step 3: for each subset digit d, pin some peer-of-q outside U
        // (and not in must_be_empty) as a given so d is eliminated from
        // q's candidates.
        let mut must_be_given: Vec<usize> = Vec::with_capacity(size);
        let mut step3_ok = true;
        for &d in &subset_digits {
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

        // Build the seeded board: the solution with the subset cells, q, and
        // the must_be_empty cells cleared.
        let mut board = solution.clone();
        for &c in &subset_cells {
            board.clear(c);
        }
        board.clear(q);
        for &c in &must_be_empty {
            board.clear(c);
        }

        // Strippable cells: not pinned (must_be_given) and not in U. U cells
        // outside the template stay as givens to preserve the "exactly size+1
        // empties in U" invariant; the must_be_given pins keep the subset
        // digits off q.
        let mut strippable: Vec<usize> = (0..CELLS)
            .filter(|i| !board.is_empty(*i))
            .filter(|i| !must_be_given.contains(i))
            .filter(|i| !unit.contains(i))
            .collect();
        rng.shuffle(&mut strippable);

        let geom = SeedGeometry {
            unit_kind,
            unit_index,
            subset_cells: subset_cells.clone(),
            q,
        };

        // One strip-down pass (no multi-shuffle retry — measured useless: the
        // passes are independent, so re-shuffling the same template just trades
        // attempts for passes at constant total work).
        for i in strippable {
            let mut candidate = board.clone();
            candidate.clear(i);
            // Cheap uniqueness reject first (bounded backtrack) before the
            // technique walks.
            if uniqueness::count_solutions(&candidate, 2) != 1 {
                continue;
            }
            // Fast path: walk without target. If it solves, target isn't
            // required — commit the strip and continue.
            if solvable_with(&candidate, allowed_without_target) {
                board = candidate;
                continue;
            }
            // Without-target walk got stuck. Confirm the candidate is still
            // allowed-solvable, then we have a Forced(target) hit.
            if solvable_with(&candidate, allowed) {
                return Some((candidate, geom));
            }
            // Not solvable at all — skip this strip.
        }
    }
    None
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
