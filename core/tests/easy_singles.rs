use sudoku_core::{Board, solve};

#[test]
fn solves_norvig_easy() {
    let puzzle = "003020600900305001001806400008102900700000008006708200002609500800203009005010300";
    let board = Board::parse(puzzle).expect("parse puzzle");
    let result = solve(board);

    if !result.solved {
        eprintln!(
            "trace length: {}, final board:\n{}",
            result.trace.len(),
            result.board
        );
    }
    assert!(
        result.solved,
        "expected Norvig easy puzzle to solve with naked + hidden singles"
    );
}
