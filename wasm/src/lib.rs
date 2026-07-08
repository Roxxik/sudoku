//! Browser bridge over `sudoku-core`.
//!
//! The UI builds a [`Board`] from the placed digits it already tracks, then
//! calls [`hint`] to get *every* step the solver can currently see. Each step
//! crosses the boundary as a plain JS object (via `serde_wasm_bindgen`) whose
//! fields mirror `core::techniques::Step` — the UI owns all string formatting
//! (cell names, technique labels, the hint sentence).
//!
//! Pencil marks are normally not part of the board: a hint reflects the true
//! position, and the solver derives the real candidates from the placements
//! itself. The optional marks-aware mode ([`Board::new`]'s second argument) is
//! additive — a per-cell candidate bitmask whose missing candidates are
//! `eliminate`d after placing — so the solver reasons over a *reduced* candidate
//! set. The web app uses it (cheat only) to forward the player's pencil marks so
//! "Apply easiest" can progress past singles: each technique elimination narrows
//! the marks, which the next hint then sees.

use serde::Serialize;
use sudoku_core::board::{ALL_DIGITS, UnitKind};
use sudoku_core::{Deduction, Step, all_techniques};
use wasm_bindgen::prelude::*;

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
    /// `candidates` is the optional marks-aware input: a per-cell 9-bit mask
    /// (bit `d-1` set if digit `d` is a candidate). For an empty cell with a
    /// non-zero mask, every grid-derived candidate the mask omits is
    /// `eliminate`d, so the solver reasons over the player's reduced set. A zero
    /// mask (or `undefined` for the whole argument) means "unspecified": keep the
    /// grid-derived candidates. Since `eliminate` only removes, a mask can never
    /// add a candidate the placements forbid — the grid still dominates.
    #[wasm_bindgen(constructor)]
    pub fn new(cells: &[u8], candidates: Option<Box<[u16]>>) -> Result<Board, JsValue> {
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
        if let Some(masks) = candidates {
            if masks.len() != 81 {
                return Err(JsValue::from_str(&format!(
                    "expected 81 candidate masks, got {}",
                    masks.len()
                )));
            }
            for (i, &mask) in masks.iter().enumerate() {
                if mask == 0 || cells[i] != 0 {
                    continue;
                }
                let keep = mask & ALL_DIGITS;
                let remove = inner.candidates(i) & !keep;
                for d in 1..=9u8 {
                    if remove & (1u16 << (d - 1)) != 0 {
                        inner.eliminate(i, d);
                    }
                }
            }
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

/// Adopt an imported puzzle from its clue line alone. `line` is the `to_line()`
/// format the app exports (81 chars, `.`/`0` empty, `1`-`9` clues; whitespace and
/// the `|`/`-`/`+` separators are ignored). Returns the same
/// `{ puzzle, solution, givens }` shape as [`generate`], with the solution solved
/// here, so the UI can load it as a game and detect a win.
///
/// Errors if the line is malformed or the puzzle lacks a unique solution. Parse
/// robustness past the exported format is a later concern — this only has to
/// re-accept what Export produces.
#[wasm_bindgen(js_name = solveLine)]
pub fn solve_line(line: &str) -> Result<JsValue, JsValue> {
    let board = sudoku_core::Board::parse(line)
        .map_err(|e| JsValue::from_str(&format!("could not read puzzle: {e}")))?;
    // Require a UNIQUE solution: collect up to two completions and insist on
    // exactly one. (`solve_unique` despite its name returns the *first* solution,
    // so an under-constrained grid -- e.g. an empty one -- would otherwise be
    // accepted.) Loosening this to accept multi-solution puzzles is a later step.
    let mut solutions = sudoku_core::uniqueness::collect_solutions(&board, 2);
    let solution = match solutions.len() {
        1 => solutions.pop().unwrap(),
        0 => return Err(JsValue::from_str("this puzzle has no solution")),
        _ => return Err(JsValue::from_str("this puzzle has more than one solution")),
    };
    let givens = (0..81).filter(|&i| !board.is_empty(i)).count() as u32;
    let data = PuzzleData {
        puzzle: board.to_line(),
        solution: solution.to_line(),
        givens,
    };
    serde_wasm_bindgen::to_value(&data).map_err(|e| JsValue::from_str(&e.to_string()))
}

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

fn step_data(s: &Step) -> StepData {
    StepData {
        technique: TechniqueData {
            id: s.technique.cli_name(),
            name: s.technique.name(),
            // Player-facing curriculum score; the UI buckets it into a tier.
            difficulty: s.technique.difficulty(),
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
