pub mod board;
pub mod composer;
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
pub use generator::{
    FilteredResult, GeneratedPuzzle, make_puzzle, make_puzzle_forced, make_puzzle_needing,
    random_full_grid,
};
pub use rng::Rng;
pub use solver::{
    SolveResult, all_techniques, apply_step, deduction_counts, max_technique, next_step,
    next_step_filtered, solve,
};
pub use spec::{Spec, Usage};
pub use techniques::{Deduction, HouseRef, REGISTRY, Step, TechniqueDef, TechniqueKind};
pub use verifier::{Violation, verify};
