pub mod board;
pub mod composer;
pub mod curriculum;
pub mod generator;
pub mod rng;
pub mod solver;
pub mod spec;
pub mod techniques;
pub mod uniqueness;
pub mod util;
pub mod verifier;

pub use board::{Board, cell_name};
pub use composer::{Constructor, HiddenTripleConstructor, compose, construct_with};
pub use curriculum::{CURRICULUM, Stage, Tier, stage_by_key};
pub use generator::{
    FilteredResult, GeneratedPuzzle, make_puzzle, make_puzzle_for_spec, make_puzzle_forced,
    make_puzzle_needing, random_full_grid,
};
pub use rng::Rng;
pub use solver::{
    SolveResult, all_techniques, all_techniques_filtered, apply_step, deduction_counts,
    deduction_counts_filtered, max_technique, next_step, next_step_filtered, solve,
    solve_filtered,
};
pub use spec::{RequireAny, Spec, Usage};
pub use techniques::{
    Deduction, Family, HouseRef, REGISTRY, Step, TechniqueDef, TechniqueKind,
};
pub use verifier::{Violation, verify};
