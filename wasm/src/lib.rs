//! Browser bridge over `sudoku-core`.
//!
//! The UI builds a [`Board`] from the placed digits it already tracks, then
//! calls [`hint`] to get *every* step the solver can currently see. Each step
//! crosses the boundary as a plain JS object (via `serde_wasm_bindgen`) whose
//! fields mirror `core::techniques::Step` — the UI owns all string formatting
//! (cell names, technique labels, the hint sentence).
//!
//! Pencil marks are intentionally not part of the board: a hint reflects the
//! true position, and the solver derives the real candidates from the
//! placements itself. A future marks-aware mode would be an additive input (a
//! per-cell candidate bitmask applied after the placements) rather than a
//! change to this path — see [`Board::new`].

use serde::Serialize;
use sudoku_core::board::UnitKind;
use sudoku_core::{Deduction, Rng, Spec, Step, Tier, all_techniques, make_puzzle_for_spec};
use sudoku_core::lab;
use wasm_bindgen::prelude::*;

/// Rejection-sampling budget for generation. Easy puzzles essentially never
/// exhaust this; harder tiers might, but those aren't wired up yet.
const MAX_ATTEMPTS: usize = 10_000;

/// A Sudoku position the UI hands to [`hint`]. Constructed from the 81 placed
/// digits (givens plus whatever the player entered); the solver figures out the
/// candidates on its own.
#[wasm_bindgen]
pub struct Board {
    inner: sudoku_core::Board,
}

#[wasm_bindgen]
impl Board {
    /// Build from 81 placements, row-major, `0` for an empty cell. Throws if the
    /// length is wrong or the placements form an illegal position (a digit
    /// repeated in a row/column/box).
    ///
    /// A marks-aware mode would take a second argument here — a per-cell
    /// candidate mask — and `eliminate` the missing candidates after placing.
    #[wasm_bindgen(constructor)]
    pub fn new(cells: &[u8]) -> Result<Board, JsValue> {
        if cells.len() != 81 {
            return Err(JsValue::from_str(&format!(
                "expected 81 cells, got {}",
                cells.len()
            )));
        }
        let mut inner = sudoku_core::Board::empty();
        for (i, &d) in cells.iter().enumerate() {
            if d == 0 {
                continue;
            }
            if d > 9 {
                return Err(JsValue::from_str(&format!(
                    "cell {} has invalid digit {}",
                    i, d
                )));
            }
            let bit = 1u16 << (d as u16 - 1);
            if inner.candidates(i) & bit == 0 {
                return Err(JsValue::from_str(&format!(
                    "digit {} at cell {} conflicts with a peer",
                    d, i
                )));
            }
            inner.place(i, d);
        }
        Ok(Board { inner })
    }

    /// Whether every cell is filled (a complete grid). The UI uses this to tell
    /// an empty step list "already solved" apart from "stuck".
    #[wasm_bindgen(js_name = isSolved)]
    pub fn is_solved(&self) -> bool {
        self.inner.is_solved()
    }
}

/// Every step the implemented techniques can find on `board`, easiest-first
/// (techniques are tried in difficulty order). Empty if the board is solved or
/// no technique applies — disambiguate with [`Board::isSolved`].
#[wasm_bindgen]
pub fn hint(board: &Board) -> Result<JsValue, JsValue> {
    let steps: Vec<StepData> = all_techniques(&board.inner).iter().map(step_data).collect();
    serde_wasm_bindgen::to_value(&steps).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Generate a fresh Tier::Beginner (hidden-singles-only) puzzle. Returns
/// `{ puzzle, solution, givens }`, the two grids as 81-char lines (`.` for an
/// empty cell) plus the clue count.
///
/// `seed` comes from JS (`Rng::from_entropy` relies on `SystemTime`, which
/// traps on wasm); pass a fresh random u64 each call. Beginner generation is
/// fast; other tiers can be slow and aren't exposed here yet.
#[wasm_bindgen]
pub fn generate(seed: u64) -> Result<JsValue, JsValue> {
    let mut rng = Rng::from_seed(seed);
    let spec = Spec::tier(Tier::Beginner);
    let fr = make_puzzle_for_spec(&mut rng, &spec, MAX_ATTEMPTS)
        .ok_or_else(|| JsValue::from_str("could not generate a puzzle within the attempt budget"))?;
    let data = PuzzleData {
        puzzle: fr.puzzle.puzzle.to_line(),
        solution: fr.puzzle.solution.to_line(),
        givens: fr.puzzle.givens as u32,
    };
    serde_wasm_bindgen::to_value(&data).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Generate a fresh puzzle with the new `generator-lab` scalar generator (the
/// tuned `random`-method path core re-exports via [`lab`]), instead of core's own
/// [`make_puzzle_for_spec`]. Returns the same `{ puzzle, solution, givens }` shape
/// as [`generate`], so the UI loads it identically.
///
/// `target` is a `generator-lab` technique-kind index (`lab::kinds`, e.g.
/// `HIDDEN_SINGLE`, `NAKED_PAIR`, `HIDDEN_QUAD`, `X_WING`); `drill` selects
/// drill-mode (target is the *hardest* technique needed) over train-mode (target
/// may be reached, easier techniques allowed alongside). `seed` comes from JS for
/// the same reason as [`generate`] (no entropy on wasm).
///
/// The native-only SIMT path is not wired here — this is the scalar generator,
/// which is all wasm has. Harder targets can exhaust the attempt budget; that
/// surfaces as an error the same way [`generate`] reports an empty budget.
#[wasm_bindgen(js_name = generateLab)]
pub fn generate_lab(seed: u64, target: u32, drill: bool) -> Result<JsValue, JsValue> {
    let target = target as usize;
    if target >= lab::kinds::NUM {
        return Err(JsValue::from_str(&format!(
            "target kind {} out of range (0..{})",
            target,
            lab::kinds::NUM
        )));
    }
    let spec = if drill {
        lab::Spec::drill(target)
    } else {
        lab::Spec::train(target)
    };
    let mut rng = lab::Rng::from_seed(seed);
    let (generated, _stats) = lab::generate(&mut rng, &spec, MAX_ATTEMPTS);
    let generated = generated
        .ok_or_else(|| JsValue::from_str("could not generate a puzzle within the attempt budget"))?;
    let data = PuzzleData {
        puzzle: generated.puzzle.0.to_line(),
        solution: generated.solution.0.to_line(),
        givens: generated.givens as u32,
    };
    serde_wasm_bindgen::to_value(&data).map_err(|e| JsValue::from_str(&e.to_string()))
}

// ---- Wire shapes -----------------------------------------------------------
// Plain mirrors of the core types, serialized to JS objects. Cells are raw
// indices (0..81, row-major) and houses are 0-based; the UI renders R/C labels.

#[derive(Serialize)]
struct PuzzleData {
    puzzle: String,
    solution: String,
    givens: u32,
}

#[derive(Serialize)]
struct StepData {
    technique: TechniqueData,
    #[serde(rename = "focusCells")]
    focus_cells: Vec<u8>,
    house: Option<HouseData>,
    deductions: Vec<DeductionData>,
}

#[derive(Serialize)]
struct TechniqueData {
    /// Stable kebab-case identifier (e.g. `"hidden-single"`).
    id: &'static str,
    /// Canonical display name (e.g. `"hidden single"`, `"X-Wing"`).
    name: &'static str,
    difficulty: u32,
}

#[derive(Serialize)]
struct HouseData {
    /// `"row"`, `"col"`, or `"box"`.
    kind: &'static str,
    index: u8,
}

#[derive(Serialize)]
struct DeductionData {
    /// `"place"` (set a digit) or `"eliminate"` (rule a candidate out).
    kind: &'static str,
    cell: u8,
    digit: u8,
}

#[allow(deprecated)] // solver_order: pending difficulty/curriculum migration
fn step_data(s: &Step) -> StepData {
    StepData {
        technique: TechniqueData {
            id: s.technique.cli_name(),
            name: s.technique.name(),
            difficulty: s.technique.solver_order(),
        },
        focus_cells: s.focus_cells.iter().map(|&c| c as u8).collect(),
        house: s.focus_house.as_ref().map(|h| HouseData {
            kind: unit_kind_str(h.kind),
            index: h.index,
        }),
        deductions: s.deductions.iter().map(deduction_data).collect(),
    }
}

fn deduction_data(d: &Deduction) -> DeductionData {
    match *d {
        Deduction::Place { cell, digit } => DeductionData {
            kind: "place",
            cell: cell as u8,
            digit,
        },
        Deduction::Eliminate { cell, digit } => DeductionData {
            kind: "eliminate",
            cell: cell as u8,
            digit,
        },
    }
}

fn unit_kind_str(k: UnitKind) -> &'static str {
    match k {
        UnitKind::Row => "row",
        UnitKind::Col => "col",
        UnitKind::Box => "box",
    }
}
