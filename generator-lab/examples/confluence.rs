//! Differential **confluence** test for the logic solver's harder ladder.
//!
//! The strip loop keeps a puzzle on *fixpoint* properties — is it solvable with the
//! toolbox? is it still stuck once the forced kind is removed? — never on which order
//! the solver happened to try techniques. So reordering the ladder must not change the
//! solve fixpoint. This harness proves it for real puzzles: it generates puzzles for a
//! set of target specs and, for each, re-solves under permutations of the harder block
//! (subsets / fish / wings) and asserts the resulting board (placements + candidates)
//! is byte-identical to the production order's.
//!
//! It tests several toolboxes per puzzle, including the *necessity* toolbox (full minus
//! the forced kind) — the deliberately non-closed set where the only known
//! non-confluence (e.g. locked-candidates without hidden singles) could bite. As a
//! sensitivity control it also runs a toolbox with the singles removed, which SHOULD
//! diverge on some boards; a control that never diverges would mean the harness is blind.
//!
//! Usage:
//!   cargo run --release -p generator-lab --example confluence -- \
//!       [--target NAME ...] [--mode train|drill] [--count K=8] [--seed BASE=1]
//!
//! NAME is any kind from `spec::kinds::NAMES`. With no `--target`, a spread of
//! subset/fish/wing targets is used.

use generator_lab::expert_spec;
use generator_lab::fingerprint::{FNV_OFFSET, FNV_PRIME};
use generator_lab::generate::random_simt::find_puzzles;
use generator_lab::repr::Board;
use generator_lab::solve::{
    HARD_STEPS_DEFAULT, HardStep, LogicSolver, Solver, solve_fixpoint_with_order,
};
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{HIDDEN_SINGLE, NAKED_SINGLE, NAMES, NUM};

/// FNV fold of a solved-or-stuck board's full state: per cell, its placed digit (0 = empty,
/// 1..=9) in the high bits and its 9-bit candidate mask in the low — so two fixpoints
/// match iff every cell agrees on placement AND remaining candidates.
fn fixpoint_fp(b: &Board) -> u64 {
    let mut h = FNV_OFFSET;
    for c in 0..81 {
        let placed = b.cell(c).map(|d| d.index() as u16 + 1).unwrap_or(0); // 0..=9
        let code = (placed << 9) | b.marks(c).bits(); // disjoint fields
        h ^= code as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// The harder-block permutations to probe. Each is the same 8 steps as
/// [`HARD_STEPS_DEFAULT`] in a different order; the aggressive ones (wings-first,
/// reverse) put the non-monotone wings before the subsets/fish they'd normally follow.
fn permutations() -> Vec<(&'static str, Vec<HardStep>)> {
    use HardStep::*;
    let default = HARD_STEPS_DEFAULT.to_vec();
    let reversed: Vec<HardStep> = HARD_STEPS_DEFAULT.iter().rev().copied().collect();
    let wings_first = vec![
        Wings,
        NakedSubset(2),
        HiddenSubset(2),
        NakedSubset(3),
        HiddenSubset(3),
        NakedSubset(4),
        HiddenSubset(4),
        Fish,
    ];
    let fish_first = vec![
        Fish,
        Wings,
        NakedSubset(2),
        HiddenSubset(2),
        NakedSubset(3),
        HiddenSubset(3),
        NakedSubset(4),
        HiddenSubset(4),
    ];
    // Hidden-before-naked at every size, sizes descending — a thorough interleave shuffle.
    let interleaved = vec![
        HiddenSubset(4),
        NakedSubset(4),
        Wings,
        HiddenSubset(3),
        NakedSubset(3),
        Fish,
        HiddenSubset(2),
        NakedSubset(2),
    ];
    vec![
        ("default", default),
        ("reversed", reversed),
        ("wings-first", wings_first),
        ("fish-first", fish_first),
        ("interleaved", interleaved),
    ]
}

/// The toolboxes to test a puzzle's solve under: the full baseline, the necessity
/// toolbox (minus the forced kind), and full-minus-each-in-scope-kind (broad stall
/// coverage). Singles stay in every one — they are the cover that keeps the set
/// confluent — so all of these MUST be order-invariant.
fn confluent_toolboxes(spec: &Spec) -> Vec<(String, u32)> {
    let full = spec.baseline_mask();
    let forced = spec.forced_mask();
    let mut out = vec![("full".to_string(), full), ("-forced".to_string(), full & !forced)];
    for idx in 0..NUM {
        // Removing a single would leave the cover-less regime — that is the control, not
        // a confluent case; skip the singles here.
        if idx == NAKED_SINGLE || idx == HIDDEN_SINGLE {
            continue;
        }
        if full & (1 << idx) != 0 {
            out.push((format!("-{}", NAMES[idx]), full & !(1u32 << idx)));
        }
    }
    out
}

fn main() {
    let mut targets: Vec<String> = Vec::new();
    let mut drill = false;
    let mut count = 8u64;
    let mut base = 1u64;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => {
                if let Some(t) = it.next() {
                    targets.push(t);
                }
            }
            "--mode" => drill = matches!(it.next().as_deref(), Some("drill")),
            "--count" => count = it.next().and_then(|s| s.parse().ok()).unwrap_or(count),
            "--seed" => base = it.next().and_then(|s| s.parse().ok()).unwrap_or(base),
            _ => {}
        }
    }
    if targets.is_empty() {
        targets = ["naked-triple", "naked-quad", "x-wing", "swordfish", "jellyfish", "xy-wing", "xyz-wing", "w-wing"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    }

    let perms = permutations();
    let mode = if drill { "drill" } else { "train" };
    println!(
        "confluence[{mode}]: {} target(s), count={count}, {} permutation(s)",
        targets.len(),
        perms.len(),
    );

    let mut total_checks = 0u64; // (puzzle, toolbox, non-default perm) triples verified
    let mut total_divergences = 0u64;
    let mut divergences: Vec<String> = Vec::new(); // capped sample of witnesses
    // Sensitivity control: solving with the singles removed should diverge on SOME board.
    let mut control_total = 0u64;
    let mut control_diverged = 0u64;

    for tname in &targets {
        let Some(target) = NAMES.iter().position(|&n| n == tname.as_str()) else {
            eprintln!("unknown target {tname:?}; known: {}", NAMES.join(", "));
            std::process::exit(2);
        };
        let spec = expert_spec(target, drill);
        let toolboxes = confluent_toolboxes(&spec);
        let control_mask = spec.baseline_mask() & !(1u32 << NAKED_SINGLE) & !(1u32 << HIDDEN_SINGLE);

        // Collect the puzzles for this target (W=8 parallel harvest).
        let mut puzzles: Vec<(u64, Board)> = Vec::new();
        find_puzzles(base..base + count, &spec, |seed, p| {
            puzzles.push((seed, Board::from_digits(&p.puzzle.0)));
        });

        let mut target_div = 0u64;
        for (seed, board) in &puzzles {
            for (tbname, mask) in &toolboxes {
                // The reference fixpoint = production order.
                let want_board = solve_fixpoint_with_order::<Board>(board, *mask, &HARD_STEPS_DEFAULT);
                // Cross-check the reference against the real production solver: its
                // solved-verdict must match, or our default-order harness is itself wrong
                // (and the whole comparison would be against a bogus reference).
                let oracle = LogicSolver::solve_tracked(board, *mask).solved;
                if want_board.is_complete() != oracle {
                    eprintln!(
                        "BUG: harness default order disagrees with LogicSolver on solved (target={tname} seed={seed} toolbox={tbname})"
                    );
                    std::process::exit(3);
                }
                let want = fixpoint_fp(&want_board);
                for (pname, perm) in &perms[1..] {
                    // skip "default" (it IS the reference)
                    let got = fixpoint_fp(&solve_fixpoint_with_order::<Board>(board, *mask, perm));
                    total_checks += 1;
                    if got != want {
                        target_div += 1;
                        total_divergences += 1;
                        if divergences.len() < 12 {
                            divergences.push(format!(
                                "  DIVERGE target={tname} seed={seed} toolbox={tbname} perm={pname}: {want:#018x} != {got:#018x}\n    puzzle: {}",
                                board.digits().to_line(),
                            ));
                        }
                    }
                }
            }
            // Control: singles removed — compare default vs reversed; expect some divergence.
            let cref = fixpoint_fp(&solve_fixpoint_with_order::<Board>(board, control_mask, &HARD_STEPS_DEFAULT));
            let crev = fixpoint_fp(&solve_fixpoint_with_order::<Board>(
                board,
                control_mask,
                &perms[1].1, // reversed
            ));
            control_total += 1;
            if cref != crev {
                control_diverged += 1;
            }
        }
        println!(
            "  {tname}: {} puzzle(s), {} toolbox(es) x {} perms -> {}",
            puzzles.len(),
            toolboxes.len(),
            perms.len() - 1,
            if target_div == 0 { "OK".to_string() } else { format!("{target_div} DIVERGENCE(S)") },
        );
    }

    println!();
    for d in &divergences {
        println!("{d}");
    }
    println!(
        "control (singles removed, expected to diverge): {control_diverged}/{control_total} boards diverged{}",
        if control_diverged == 0 { "  [WARNING: harness may be insensitive]" } else { "  [harness is sensitive]" },
    );
    println!("SUMMARY: {total_checks} confluent (puzzle,toolbox,perm) checks, {total_divergences} divergence(s)");
    if total_divergences > 0 {
        println!("RESULT: NON-CONFLUENT — reordering changes the fixpoint (see witnesses above)");
        std::process::exit(1);
    }
    println!("RESULT: CONFLUENT — every permutation reached the identical fixpoint on every board/toolbox");
}
