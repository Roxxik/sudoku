use std::collections::HashSet;
use std::io::{BufRead, Write};
use sudoku_core::{
    Board, CURRICULUM, Deduction, Family, GeneratedPuzzle, HiddenTripleConstructor, HouseRef,
    Probe, REGISTRY, Rng, Spec, Step, TechniqueKind, Tier, all_techniques, all_techniques_filtered,
    apply_step, cell_name, construct_with, deduction_counts, deduction_counts_filtered,
    make_puzzle, make_puzzle_for_spec_with_search, make_puzzle_forced, make_puzzle_needing,
    max_technique, solve, solve_filtered, stage_by_key,
};

const MAX_ATTEMPTS: usize = 10_000;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let interactive = pop_flag(&mut args, "--interactive") || pop_flag(&mut args, "-i");
    let list_stages = pop_flag(&mut args, "--list-stages");
    let full_trace = pop_flag(&mut args, "--full-trace");

    // Legacy single-purpose flags.
    let needs = pop_string(&mut args, "--needs");
    let forced = pop_string(&mut args, "--forced");
    let construct = pop_string(&mut args, "--construct");

    // Probe: dump the avoid-walk trace for the first N substitutable no-req
    // seeds the generator hits (debugging why a technique isn't forced).
    let probe_trace = pop_string(&mut args, "--probe-trace");
    // Probe: run targeted-strip forcing on the first N substitutable no-req
    // seeds and print the survival-score progression (does it reach FORCED?).
    let probe_force = pop_string(&mut args, "--probe-force");

    // Local-search budget for spec generation (0 = off, the legacy
    // rejection-only behavior). Mirrors the bench's --search-iters.
    let search_iters = parse_probe_count(pop_string(&mut args, "--search-iters").as_deref(), "--search-iters");
    let resume_threshold = pop_string(&mut args, "--resume-threshold")
        .map(|s| s.parse::<i64>().unwrap_or_else(|_| {
            eprintln!("--resume-threshold needs an integer");
            std::process::exit(2);
        }));

    // Spec-driven flags.
    let stage = pop_string(&mut args, "--stage");
    let train = pop_string(&mut args, "--train");
    let drill = pop_string(&mut args, "--drill");
    let tier_arg = pop_string(&mut args, "--tier");
    let family_arg = pop_string(&mut args, "--family");
    let allow_arg = pop_string(&mut args, "--allow");
    let require_arg = pop_string(&mut args, "--require");

    if list_stages {
        print_stages();
        return;
    }

    let seed: Option<u64> = match args.as_slice() {
        [] => None,
        [s] => match s.parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => usage_error(),
        },
        _ => usage_error(),
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

    let spec_starts: usize = [&stage, &train, &drill, &tier_arg]
        .iter()
        .filter(|s| s.is_some())
        .count();
    if spec_starts > 1 {
        eprintln!("--stage, --train, --drill, --tier are mutually exclusive");
        std::process::exit(2);
    }
    let legacy: usize = [&construct, &needs, &forced]
        .iter()
        .filter(|s| s.is_some())
        .count();
    if legacy > 0
        && (spec_starts > 0 || family_arg.is_some() || allow_arg.is_some() || require_arg.is_some())
    {
        eprintln!("--construct/--needs/--forced cannot be combined with --stage/--train/--drill/--tier/--family/--allow/--require");
        std::process::exit(2);
    }

    // Probe mode: don't generate a puzzle to display — run the generator with
    // the trace and/or forcing probe so it dumps example substitutable
    // positions (and forcing attempts) along the way, then exit.
    if probe_trace.is_some() || probe_force.is_some() {
        let trace = parse_probe_count(probe_trace.as_deref(), "--probe-trace");
        let force = parse_probe_count(probe_force.as_deref(), "--probe-force");
        if spec_starts == 0
            && family_arg.is_none()
            && allow_arg.is_none()
            && require_arg.is_none()
        {
            eprintln!("--probe-trace/--probe-force need a spec (e.g. --drill <technique>)");
            std::process::exit(2);
        }
        let spec = build_spec(
            stage.as_deref(),
            train.as_deref(),
            drill.as_deref(),
            tier_arg.as_deref(),
            family_arg.as_deref(),
            allow_arg.as_deref(),
            require_arg.as_deref(),
        );
        println!(
            "probe: up to {} trace dump(s), {} forcing attempt(s) on substitutable no-req seeds",
            trace, force,
        );
        let (out, stats) = make_puzzle_for_spec_with_search(
            &mut rng,
            &spec,
            MAX_ATTEMPTS,
            0,
            None,
            Probe { classify: false, trace, force },
        );
        println!(
            "no-req attempts seen: {} (of {} total)",
            stats.requirement_never_fired, stats.attempts,
        );
        match out {
            Some(fr) => println!("generation also succeeded after {} attempts", fr.attempts),
            None => println!("generation did not succeed in {} attempts", MAX_ATTEMPTS),
        }
        return;
    }

    let (out, display_spec): (GeneratedPuzzle, Option<Spec>) =
        if let Some(name) = construct.as_ref() {
            let (puzzle, spec) = run_construct(name, &mut rng);
            (puzzle, Some(spec))
        } else if let Some(name) = needs.as_ref() {
            (run_needs(name, &mut rng), None)
        } else if let Some(name) = forced.as_ref() {
            (run_forced(name, &mut rng), None)
        } else if spec_starts > 0
            || family_arg.is_some()
            || allow_arg.is_some()
            || require_arg.is_some()
        {
            let spec = build_spec(
                stage.as_deref(),
                train.as_deref(),
                drill.as_deref(),
                tier_arg.as_deref(),
                family_arg.as_deref(),
                allow_arg.as_deref(),
                require_arg.as_deref(),
            );
            let puzzle = run_spec(&spec, &mut rng, search_iters, resume_threshold);
            (puzzle, Some(spec))
        } else {
            (make_puzzle(&mut rng, true), None)
        };

    // By default, restrict the displayed solve to whatever spec was in play —
    // so a drill puzzle shows the drill path, not the canonical (cheapest-first)
    // path that may use forbidden techniques. `--full-trace` opts out.
    let restrict = if full_trace { None } else { display_spec.as_ref() };

    if interactive {
        run_interactive(out, restrict);
    } else {
        run_batch(out, restrict);
    }
}

fn usage_error() -> ! {
    eprintln!(
        "usage: sudoku [--interactive] [--list-stages] \\\n\
         \t[--stage <key> | --train <technique> | --drill <technique> | --tier <easy|medium|hard|master>] \\\n\
         \t[--family <name>] [--allow t1,t2,...] [--require t=n,t=n,...] \\\n\
         \t[--construct <name> | --needs <technique> | --forced <technique>] \\\n\
         \t[seed]",
    );
    std::process::exit(2);
}

fn pop_flag(args: &mut Vec<String>, name: &str) -> bool {
    if let Some(p) = args.iter().position(|a| a == name) {
        args.remove(p);
        true
    } else {
        false
    }
}

fn parse_probe_count(arg: Option<&str>, name: &str) -> usize {
    match arg {
        None => 0,
        Some(s) => s.parse().unwrap_or_else(|_| {
            eprintln!("{} needs a non-negative integer", name);
            std::process::exit(2);
        }),
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

fn parse_family(name: &str) -> Option<Family> {
    match name {
        "singles" => Some(Family::Singles),
        "locked-candidates" => Some(Family::LockedCandidates),
        "naked-subset" => Some(Family::NakedSubset),
        "hidden-subset" => Some(Family::HiddenSubset),
        "fish" => Some(Family::Fish),
        "turbot-fish" => Some(Family::TurbotFish),
        "wing" => Some(Family::Wing),
        "finned-fish" => Some(Family::FinnedFish),
        _ => None,
    }
}

fn family_name(f: Family) -> &'static str {
    match f {
        Family::Singles => "singles",
        Family::LockedCandidates => "locked-candidates",
        Family::NakedSubset => "naked-subset",
        Family::HiddenSubset => "hidden-subset",
        Family::Fish => "fish",
        Family::TurbotFish => "turbot-fish",
        Family::Wing => "wing",
        Family::FinnedFish => "finned-fish",
    }
}

fn build_spec(
    stage: Option<&str>,
    train: Option<&str>,
    drill: Option<&str>,
    tier_arg: Option<&str>,
    family_arg: Option<&str>,
    allow_arg: Option<&str>,
    require_arg: Option<&str>,
) -> Spec {
    let mut spec = if let Some(key) = stage {
        let s = stage_by_key(key).unwrap_or_else(|| {
            eprintln!("unknown stage {:?} (try --list-stages)", key);
            std::process::exit(2);
        });
        Spec::for_stage(s)
    } else if let Some(name) = train {
        let t = parse_technique(name).unwrap_or_else(|| {
            eprintln!("unknown technique {:?}", name);
            std::process::exit(2);
        });
        Spec::train(t)
    } else if let Some(name) = drill {
        let t = parse_technique(name).unwrap_or_else(|| {
            eprintln!("unknown technique {:?}", name);
            std::process::exit(2);
        });
        Spec::drill(t)
    } else if let Some(t) = tier_arg {
        let tier = Tier::from_key(t).unwrap_or_else(|| {
            eprintln!("unknown tier {:?} (easy|medium|hard|master)", t);
            std::process::exit(2);
        });
        Spec::tier(tier)
    } else {
        Spec::empty()
    };

    if let Some(name) = family_arg {
        let f = parse_family(name).unwrap_or_else(|| {
            eprintln!(
                "unknown family {:?} (singles|locked-candidates|naked-subset|hidden-subset|fish|turbot-fish|wing|finned-fish)",
                name
            );
            std::process::exit(2);
        });
        spec = spec.require_family(f, 1);
    }

    if let Some(list) = allow_arg {
        for tok in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let t = parse_technique(tok).unwrap_or_else(|| {
                eprintln!("unknown technique in --allow: {:?}", tok);
                std::process::exit(2);
            });
            spec = spec.allow(t);
        }
    }

    if let Some(list) = require_arg {
        for tok in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (name, count) = parse_require_token(tok);
            let t = parse_technique(name).unwrap_or_else(|| {
                eprintln!("unknown technique in --require: {:?}", name);
                std::process::exit(2);
            });
            spec = spec.require(t, count);
        }
    }

    spec
}

fn parse_require_token(tok: &str) -> (&str, usize) {
    match tok.split_once('=') {
        Some((name, n)) => {
            let count = n.parse::<usize>().unwrap_or_else(|_| {
                eprintln!("--require count must be a positive integer, got {:?}", n);
                std::process::exit(2);
            });
            (name, count)
        }
        None => (tok, 1),
    }
}

fn print_stages() {
    println!("Training stages (use with --stage <key>):");
    println!("  {:<22} {:<32} {}", "key", "label", "focus");
    for s in CURRICULUM {
        println!(
            "  {:<22} {:<32} {} (diff {})",
            s.key,
            s.label,
            s.focus.cli_name(),
            s.focus.difficulty(),
        );
    }
    println!();
    println!("Tiers (use with --tier):");
    for &t in Tier::ALL {
        let ceiling = match t.ceiling() {
            Some(c) => format!("through {} (diff {})", c.cli_name(), c.difficulty()),
            None => "no cap".to_string(),
        };
        println!("  {:<8} {}", t.key(), ceiling);
    }
    println!();
    println!("Families (use with --family):");
    let mut printed: HashSet<Family> = HashSet::new();
    for d in REGISTRY {
        if printed.insert(d.family) {
            let members: Vec<&'static str> = d
                .family
                .members()
                .iter()
                .map(|k| k.cli_name())
                .collect();
            println!("  {:<18} {}", family_name(d.family), members.join(", "));
        }
    }
}

fn run_construct(name: &str, rng: &mut Rng) -> (GeneratedPuzzle, Spec) {
    match name {
        "hidden-triple" => {
            let spec = Spec::allow_up_to(TechniqueKind::HiddenSingle)
                .require(TechniqueKind::HiddenTriple, 1);
            let constructor = HiddenTripleConstructor::for_spec(spec.clone());
            match construct_with(&constructor, rng, 1000) {
                Some((board, attempts)) => {
                    println!("constructed after {} attempts", attempts);
                    let solution = solve(board.clone()).board;
                    let givens = (0..81).filter(|&i| !board.is_empty(i)).count();
                    (
                        GeneratedPuzzle {
                            puzzle: board,
                            solution,
                            givens,
                        },
                        spec,
                    )
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
}

fn run_needs(name: &str, rng: &mut Rng) -> GeneratedPuzzle {
    let target = parse_technique(name).unwrap_or_else(|| {
        eprintln!("unknown technique name {:?}", name);
        std::process::exit(2);
    });
    match make_puzzle_needing(rng, target, MAX_ATTEMPTS) {
        Some(fr) => {
            println!("found after {} attempts", fr.attempts);
            fr.puzzle
        }
        None => {
            eprintln!(
                "no puzzle requiring {} found in {} attempts",
                target.name(),
                MAX_ATTEMPTS
            );
            std::process::exit(1);
        }
    }
}

fn run_forced(name: &str, rng: &mut Rng) -> GeneratedPuzzle {
    let target = parse_technique(name).unwrap_or_else(|| {
        eprintln!("unknown technique name {:?}", name);
        std::process::exit(2);
    });
    match make_puzzle_forced(rng, target, MAX_ATTEMPTS) {
        Some(fr) => {
            println!("forced after {} attempts", fr.attempts);
            fr.puzzle
        }
        None => {
            eprintln!(
                "no puzzle FORCING {} found in {} attempts",
                target.name(),
                MAX_ATTEMPTS
            );
            std::process::exit(1);
        }
    }
}

fn run_spec(
    spec: &Spec,
    rng: &mut Rng,
    search_iters: usize,
    resume_threshold: Option<i64>,
) -> GeneratedPuzzle {
    let (out, stats) = make_puzzle_for_spec_with_search(
        rng,
        spec,
        MAX_ATTEMPTS,
        search_iters,
        resume_threshold,
        Probe::default(),
    );
    match out {
        Some(fr) => {
            println!("satisfied spec after {} attempts", fr.attempts);
            if search_iters > 0 {
                println!(
                    "  (local search: {} recoveries, {} iterations)",
                    stats.search_recoveries, stats.search_iterations,
                );
            }
            fr.puzzle
        }
        None => {
            eprintln!("no puzzle satisfying spec found in {} attempts", MAX_ATTEMPTS);
            std::process::exit(1);
        }
    }
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

fn run_batch(out: GeneratedPuzzle, restrict: Option<&Spec>) {
    println!("Puzzle ({} givens):", out.givens);
    print!("{}", out.puzzle);
    println!("line: {}", out.puzzle.to_line());
    println!();

    let (result, counts) = match restrict {
        Some(spec) => {
            // Show the baseline solve path — for drill specs that's singles + T,
            // not the broader spec scope that includes Conceded techniques.
            let allow = |t: TechniqueKind| spec.is_baseline(t);
            (
                solve_filtered(out.puzzle.clone(), allow),
                deduction_counts_filtered(&out.puzzle, allow),
            )
        }
        None => (solve(out.puzzle.clone()), deduction_counts(&out.puzzle)),
    };
    let hardest = max_technique(&result.trace)
        .map(|t| t.name().to_string())
        .unwrap_or_else(|| "none".to_string());
    let scope = match restrict {
        Some(_) => " (restricted to spec)",
        None => "",
    };
    println!(
        "Trace{}: {} steps, hardest: {}, max-branch {}",
        scope,
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
        match restrict {
            Some(_) => println!("Solved within the spec's allowed techniques."),
            None => println!("Solved with the techniques implemented so far."),
        }
    } else {
        let suffix = match restrict {
            Some(_) => "needs a technique outside the spec",
            None => "needs a technique we haven't implemented yet",
        };
        println!(
            "STUCK after {} steps — {}:",
            result.trace.len(),
            suffix,
        );
        print!("{}", result.board);
    }
}

struct Conclusion {
    deductions: Vec<Deduction>,
    sightings: Vec<(TechniqueKind, Option<HouseRef>)>,
    representative: Step,
}

fn collect_conclusions(board: &Board, restrict: Option<&Spec>) -> Vec<Conclusion> {
    let steps = match restrict {
        Some(spec) => all_techniques_filtered(board, |t| spec.is_in_scope(t)),
        None => all_techniques(board),
    };

    let mut out: Vec<Conclusion> = Vec::new();
    for step in steps {
        let sighting = (step.technique, step.focus_house);
        if let Some(c) = out.iter_mut().find(|c| c.deductions[..] == step.deductions[..]) {
            c.sightings.push(sighting);
        } else {
            out.push(Conclusion {
                deductions: step.deductions.to_vec(),
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

fn run_interactive(out: GeneratedPuzzle, restrict: Option<&Spec>) {
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

        let conclusions = collect_conclusions(&board, restrict);
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
