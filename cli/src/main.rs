use std::io::{BufRead, Write};
use sudoku_core::{
    Board, Deduction, GeneratedPuzzle, HiddenTripleConstructor, HouseRef, REGISTRY, Rng, Spec,
    Step, TechniqueKind, all_techniques, apply_step, cell_name, construct_with, deduction_counts,
    make_puzzle, make_puzzle_forced, make_puzzle_needing, max_technique, solve,
};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let interactive = pop_flag(&mut args, "--interactive") || pop_flag(&mut args, "-i");
    let needs = pop_string(&mut args, "--needs");
    let forced = pop_string(&mut args, "--forced");
    let construct = pop_string(&mut args, "--construct");
    let seed: Option<u64> = match args.as_slice() {
        [] => None,
        [s] => match s.parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => {
                eprintln!("usage: sudoku [--interactive] [seed]");
                std::process::exit(2);
            }
        },
        _ => {
            eprintln!("usage: sudoku [--interactive] [seed]");
            std::process::exit(2);
        }
    };

    let actual_seed = seed.unwrap_or_else(|| {
        let mut e = Rng::from_entropy();
        let mut s = e.next_u64();
        if s == 0 {
            s = 1;
        }
        s
    });
    println!("seed: {}", actual_seed);

    let mut rng = Rng::from_seed(actual_seed);
    let out = if let Some(name) = construct.as_ref() {
        match name.as_str() {
            "hidden-triple" => {
                let spec = Spec::allow_up_to(TechniqueKind::HiddenSingle)
                    .require(TechniqueKind::HiddenTriple, 1);
                let constructor = HiddenTripleConstructor::for_spec(spec);
                match construct_with(&constructor, &mut rng, 1000) {
                    Some((board, attempts)) => {
                        println!("constructed after {} attempts", attempts);
                        let solution = solve(board.clone()).board;
                        let givens = (0..81).filter(|&i| !board.is_empty(i)).count();
                        GeneratedPuzzle {
                            puzzle: board,
                            solution,
                            givens,
                        }
                    }
                    None => {
                        eprintln!("constructor failed within 1000 attempts");
                        std::process::exit(1);
                    }
                }
            }
            other => {
                eprintln!("no constructor for {:?}", other);
                std::process::exit(2);
            }
        }
    } else if let Some(name) = needs.as_ref() {
        let target = parse_technique(name).unwrap_or_else(|| {
            eprintln!("unknown technique name {:?}", name);
            std::process::exit(2);
        });
        match make_puzzle_needing(&mut rng, target, 10_000) {
            Some(fr) => {
                println!("found after {} attempts", fr.attempts);
                fr.puzzle
            }
            None => {
                eprintln!("no puzzle requiring {} found in 10000 attempts", target.name());
                std::process::exit(1);
            }
        }
    } else if let Some(name) = forced.as_ref() {
        let target = parse_technique(name).unwrap_or_else(|| {
            eprintln!("unknown technique name {:?}", name);
            std::process::exit(2);
        });
        match make_puzzle_forced(&mut rng, target, 10_000) {
            Some(fr) => {
                println!("forced after {} attempts", fr.attempts);
                fr.puzzle
            }
            None => {
                eprintln!("no puzzle FORCING {} found in 10000 attempts", target.name());
                std::process::exit(1);
            }
        }
    } else {
        make_puzzle(&mut rng, true)
    };

    if interactive {
        run_interactive(out);
    } else {
        run_batch(out);
    }
}

fn pop_flag(args: &mut Vec<String>, name: &str) -> bool {
    if let Some(p) = args.iter().position(|a| a == name) {
        args.remove(p);
        true
    } else {
        false
    }
}

fn pop_string(args: &mut Vec<String>, name: &str) -> Option<String> {
    let p = args.iter().position(|a| a == name)?;
    if p + 1 >= args.len() {
        eprintln!("{} needs a value", name);
        std::process::exit(2);
    }
    let raw = args.remove(p + 1);
    args.remove(p);
    Some(raw)
}

fn parse_technique(name: &str) -> Option<TechniqueKind> {
    REGISTRY
        .iter()
        .find(|d| d.cli_name == name)
        .map(|d| d.kind)
}

fn format_deductions(ds: &[Deduction]) -> String {
    ds.iter()
        .map(format_deduction)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_step(step: &Step) -> String {
    let house = step
        .focus_house
        .as_ref()
        .map(|h| format!(" [{}]", h.describe()))
        .unwrap_or_default();
    let focus = if step.focus_cells.is_empty() {
        String::new()
    } else {
        let cells: Vec<String> = step.focus_cells.iter().map(|&c| cell_name(c)).collect();
        format!(" @ {}", cells.join(","))
    };
    format!(
        "{}{}{} — {}",
        step.technique.name(),
        house,
        focus,
        format_deductions(&step.deductions),
    )
}

fn run_batch(out: GeneratedPuzzle) {
    println!("Puzzle ({} givens):", out.givens);
    print!("{}", out.puzzle);
    println!("line: {}", out.puzzle.to_line());
    println!();

    let result = solve(out.puzzle.clone());
    let counts = deduction_counts(&out.puzzle);
    let hardest = max_technique(&result.trace)
        .map(|t| t.name().to_string())
        .unwrap_or_else(|| "none".to_string());
    println!(
        "Trace: {} steps, hardest: {}, max-branch {}",
        result.trace.len(),
        hardest,
        counts.iter().copied().max().unwrap_or(0),
    );
    for (i, step) in result.trace.iter().enumerate() {
        let c = counts.get(i).copied().unwrap_or(0);
        println!("  {:>3}. [{:>2}] {}", i + 1, c, format_step(step));
    }
    println!();

    if result.solved {
        println!("Solved with the techniques implemented so far.");
    } else {
        println!(
            "STUCK after {} steps — needs a technique we haven't implemented yet:",
            result.trace.len()
        );
        print!("{}", result.board);
    }
}

struct Conclusion {
    deductions: Vec<Deduction>,
    sightings: Vec<(TechniqueKind, Option<HouseRef>)>,
    representative: Step,
}

fn collect_conclusions(board: &Board) -> Vec<Conclusion> {
    let steps = all_techniques(board);

    let mut out: Vec<Conclusion> = Vec::new();
    for step in steps {
        let sighting = (step.technique, step.focus_house);
        if let Some(c) = out.iter_mut().find(|c| c.deductions == step.deductions) {
            c.sightings.push(sighting);
        } else {
            out.push(Conclusion {
                deductions: step.deductions.clone(),
                sightings: vec![sighting],
                representative: step,
            });
        }
    }
    out
}

fn format_deduction(d: &Deduction) -> String {
    match *d {
        Deduction::Place { cell, digit } => format!("{} = {}", cell_name(cell), digit),
        Deduction::Eliminate { cell, digit } => format!("{} != {}", cell_name(cell), digit),
    }
}

fn format_sightings(sightings: &[(TechniqueKind, Option<HouseRef>)]) -> String {
    let mut grouped: Vec<(TechniqueKind, Vec<String>)> = Vec::new();
    for (tk, h) in sightings {
        let house_str = h.as_ref().map(|h| h.describe());
        if let Some(slot) = grouped.iter_mut().find(|(t, _)| t == tk) {
            if let Some(s) = house_str {
                slot.1.push(s);
            }
        } else {
            grouped.push((*tk, house_str.into_iter().collect()));
        }
    }
    grouped
        .into_iter()
        .map(|(tk, houses)| {
            if houses.is_empty() {
                tk.name().to_string()
            } else {
                format!("{} via {}", tk.name(), houses.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn format_conclusion(c: &Conclusion) -> String {
    let head = c
        .deductions
        .iter()
        .map(format_deduction)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} — {}", head, format_sightings(&c.sightings))
}

fn run_interactive(out: GeneratedPuzzle) {
    let mut board = out.puzzle.clone();
    let mut step_num = 1usize;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    println!("Puzzle ({} givens):", out.givens);

    loop {
        println!();
        println!("--- step {} ---", step_num);
        print!("{}", board);

        if board.is_solved() {
            println!("Solved!");
            return;
        }

        let conclusions = collect_conclusions(&board);
        if conclusions.is_empty() {
            println!("Stuck — no implemented technique applies. Needs more techniques.");
            return;
        }

        let total_sightings: usize = conclusions.iter().map(|c| c.sightings.len()).sum();
        println!(
            "Applicable ({} deductions, {} sightings):",
            conclusions.len(),
            total_sightings
        );
        for (i, c) in conclusions.iter().enumerate() {
            let marker = if i == 0 { ">" } else { " " };
            println!("  {} [{:>2}] {}", marker, i + 1, format_conclusion(c));
        }
        print!("  <enter>=apply [1], number=apply that one, q=quit > ");
        stdout.flush().ok();

        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() || line.is_empty() {
            return;
        }
        let input = line.trim();
        let chosen = if input.is_empty() {
            0
        } else if input.eq_ignore_ascii_case("q") {
            return;
        } else {
            match input.parse::<usize>() {
                Ok(n) if (1..=conclusions.len()).contains(&n) => n - 1,
                _ => {
                    println!("(invalid input)");
                    continue;
                }
            }
        };

        let picked = &conclusions[chosen];
        apply_step(&mut board, &picked.representative);
        println!("applied: {}", format_conclusion(picked));
        step_num += 1;
    }
}
