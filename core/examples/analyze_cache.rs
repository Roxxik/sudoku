// Analyze cached forced puzzles (core/cache/forced.jsonl, produced by the
// gen_cache example). Pays the search cost zero times — everything here reads
// the cache back. Two analyses:
//
//   1. SAMPLE SPACE — per (target, mode), and split by generation method
//      (random vs constructor): givens, baseline solve length, target uses, max
//      branch factor, and which house kind (row/col/box) carries the forced
//      target. Tells us whether two methods draw from the same population.
//
//   2. STUCK STATE — the structure a constructor would have to BUILD. For each
//      puzzle, replay the avoid-target walk (in-scope minus target) to its first
//      stall and characterize that bottleneck: how many cells are still empty,
//      how many distinct target instances are forced there, the candidate-count
//      distribution of the empty cells (tight 2s vs loose), and how many
//      conceded/single steps fired before the stall.
//
// Usage:
//   cargo run --release --example analyze_cache -- \
//     [--cache-path core/cache/forced.jsonl] [--target hidden-quad] [--mode drill]

use std::collections::BTreeSet;

use sudoku_core::board::{BOX_UNITS, COL_UNITS, ROW_UNITS, UnitKind};
use sudoku_core::{
    Board, REGISTRY, Spec, Step, TechniqueKind, all_techniques_filtered, apply_step,
    deduction_counts_filtered, next_step_filtered, solve_filtered,
};

struct Args {
    cache_path: String,
    target_filter: Option<String>,
    mode_filter: Option<String>,
}

fn parse_args() -> Args {
    let mut out = Args {
        cache_path: "core/cache/forced.jsonl".to_string(),
        target_filter: None,
        mode_filter: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cache-path" => out.cache_path = it.next().unwrap_or(out.cache_path),
            "--target" => out.target_filter = it.next(),
            "--mode" => out.mode_filter = it.next(),
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }
    out
}

struct Record {
    method: String,
    mode: String,
    target: String,
    puzzle: String,
}

/// Extract a string field `"key":"value"` from a flat JSON line.
fn field_str(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", key);
    let start = line.find(&pat)? + pat.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn parse_record(line: &str) -> Option<Record> {
    Some(Record {
        method: field_str(line, "method")?,
        mode: field_str(line, "mode")?,
        target: field_str(line, "target")?,
        puzzle: field_str(line, "puzzle")?,
    })
}

fn tech_by_cli(s: &str) -> Option<TechniqueKind> {
    REGISTRY.iter().find(|d| d.cli_name == s).map(|d| d.kind)
}

fn spec_for(target: TechniqueKind, mode: &str) -> Spec {
    match mode {
        "drill" => Spec::drill(target),
        _ => Spec::train(target),
    }
}

// ---------------------------------------------------------------------------
// Sample-space features (baseline-trace shape).
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Features {
    givens: usize,
    trace_len: usize,
    target_count: usize,
    max_branch: usize,
    target_house: Option<UnitKind>,
}

fn features(board: &Board, spec: &Spec, target: TechniqueKind) -> Features {
    let allow = |t: TechniqueKind| spec.is_baseline(t);
    let res = solve_filtered(board.clone(), allow);
    let branches = deduction_counts_filtered(board, allow);
    let first_target: Option<&Step> = res.trace.iter().find(|s| s.technique == target);
    Features {
        givens: (0..81).filter(|&i| !board.is_empty(i)).count(),
        trace_len: res.trace.len(),
        target_count: res.trace.iter().filter(|s| s.technique == target).count(),
        max_branch: branches.iter().copied().max().unwrap_or(0),
        target_house: first_target.and_then(|s| s.focus_house.as_ref()).map(|h| h.kind),
    }
}

struct Stats {
    mean: f64,
    std: f64,
    min: f64,
    max: f64,
}

fn stats(xs: &[f64]) -> Stats {
    let n = xs.len();
    if n == 0 {
        return Stats { mean: 0.0, std: 0.0, min: 0.0, max: 0.0 };
    }
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
    Stats { mean, std: var.sqrt(), min: xs.iter().cloned().fold(f64::INFINITY, f64::min), max: xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max) }
}

fn print_feature(name: &str, la: &str, a: &[f64], lb: &str, b: &[f64]) {
    let sa = stats(a);
    let sb = stats(b);
    println!(
        "  {:<12}  {:<11}: mean {:>6.2} sd {:>5.2} [{:>2.0}..{:>2.0}]   {:<11}: mean {:>6.2} sd {:>5.2} [{:>2.0}..{:>2.0}]   dmean {:+.2}",
        name, la, sa.mean, sa.std, sa.min, sa.max, lb, sb.mean, sb.std, sb.min, sb.max, sb.mean - sa.mean,
    );
}

fn house_split(fs: &[Features]) -> (usize, usize, usize) {
    let mut r = 0;
    let mut c = 0;
    let mut b = 0;
    for f in fs {
        match f.target_house {
            Some(UnitKind::Row) => r += 1,
            Some(UnitKind::Col) => c += 1,
            Some(UnitKind::Box) => b += 1,
            None => {}
        }
    }
    (r, c, b)
}

fn col(fs: &[Features], f: impl Fn(&Features) -> f64) -> Vec<f64> {
    fs.iter().map(f).collect()
}

fn pct(k: usize, n: usize) -> f64 {
    if n == 0 { 0.0 } else { 100.0 * k as f64 / n as f64 }
}

// ---------------------------------------------------------------------------
// Stuck-state characterization.
// ---------------------------------------------------------------------------

struct Stuck {
    /// Number of conceded/single steps fired before the avoid-walk stalled.
    walk_len: usize,
    /// Empty cells at the stall.
    empties: usize,
    /// Distinct forced target instances available at the stall (by house + cells).
    instances: usize,
    /// Candidate-count histogram of the empty cells at the stall (index = count,
    /// 2..=9; index 0/1 should stay empty on a stuck state).
    cand_hist: [usize; 10],
    /// True if the avoid-walk reached a genuine stall with the target available
    /// (i.e. the puzzle is actually forced from here).
    forced: bool,
    /// Empty-cell count of the unit hosting the (first) forced instance, at the
    /// stall. The naked-subset duality predicts this must be 9 for a forced
    /// hidden quad (the dual would otherwise be an in-scope naked <=4 subset that
    /// substitutes). None if not forced.
    forcing_unit_empties: Option<usize>,
    /// Distinct unit-KINDS among the forced instances at the stall. If a single
    /// stall exposes the quad in >1 kind, reporting just one biases the house
    /// split — this lets us check that "overlapping units, pick one" artifact.
    kinds: Vec<UnitKind>,
}

fn unit_cells(kind: UnitKind, index: u8) -> [usize; 9] {
    match kind {
        UnitKind::Row => ROW_UNITS[index as usize],
        UnitKind::Col => COL_UNITS[index as usize],
        UnitKind::Box => BOX_UNITS[index as usize],
    }
}

fn stuck_state(board: &Board, spec: &Spec, target: TechniqueKind) -> Stuck {
    let mut b = board.clone();
    let mut walk_len = 0;
    loop {
        if b.is_solved() {
            return Stuck { walk_len, empties: 0, instances: 0, cand_hist: [0; 10], forced: false, forcing_unit_empties: None, kinds: Vec::new() };
        }
        if let Some(s) = next_step_filtered(&b, |t| spec.is_in_scope(t) && t != target) {
            apply_step(&mut b, &s);
            walk_len += 1;
            continue;
        }
        // Stalled without target. Characterize this state.
        let mut instances: BTreeSet<(UnitKind, u8, Vec<usize>)> = BTreeSet::new();
        for s in all_techniques_filtered(&b, |t| t == target) {
            if let Some(h) = s.focus_house.as_ref() {
                let mut cells = s.focus_cells.clone();
                cells.sort_unstable();
                instances.insert((h.kind, h.index, cells));
            }
        }
        let forcing_unit_empties = instances.iter().next().map(|(kind, idx, _)| {
            unit_cells(*kind, *idx).iter().filter(|&&c| b.is_empty(c)).count()
        });
        let mut kinds: Vec<UnitKind> = instances.iter().map(|(k, _, _)| *k).collect();
        kinds.sort();
        kinds.dedup();
        let mut cand_hist = [0usize; 10];
        let mut empties = 0;
        for i in 0..81 {
            if b.is_empty(i) {
                empties += 1;
                let n = (b.candidates(i) & 0x1FF).count_ones() as usize;
                cand_hist[n.min(9)] += 1;
            }
        }
        return Stuck { walk_len, empties, instances: instances.len(), cand_hist, forced: !instances.is_empty(), forcing_unit_empties, kinds };
    }
}

fn main() {
    let args = parse_args();
    let text = match std::fs::read_to_string(&args.cache_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("could not read cache {}: {e}", args.cache_path);
            std::process::exit(1);
        }
    };

    let mut records: Vec<Record> = text.lines().filter_map(parse_record).collect();
    if let Some(t) = &args.target_filter {
        records.retain(|r| &r.target == t);
    }
    if let Some(m) = &args.mode_filter {
        records.retain(|r| &r.mode == m);
    }
    println!("loaded {} cached records from {}", records.len(), args.cache_path);
    if records.is_empty() {
        return;
    }

    // Group by (target, mode).
    let mut groups: BTreeSet<(String, String)> = BTreeSet::new();
    for r in &records {
        groups.insert((r.target.clone(), r.mode.clone()));
    }

    for (target_name, mode) in groups {
        let Some(target) = tech_by_cli(&target_name) else {
            println!("\n(unknown target {target_name} - skipping)");
            continue;
        };
        let spec = spec_for(target, &mode);
        let grp: Vec<&Record> = records.iter().filter(|r| r.target == target_name && r.mode == mode).collect();
        println!("\n================ {} ({}) : {} puzzles ================", target_name, mode, grp.len());

        // Split by method.
        let methods: BTreeSet<String> = grp.iter().map(|r| r.method.clone()).collect();
        let by_method: Vec<(String, Vec<Features>)> = methods
            .iter()
            .map(|m| {
                let fs: Vec<Features> = grp
                    .iter()
                    .filter(|r| &r.method == m)
                    .filter_map(|r| Board::parse(&r.puzzle).ok())
                    .map(|b| features(&b, &spec, target))
                    .collect();
                (m.clone(), fs)
            })
            .collect();

        println!("-- sample space (by method) --");
        for (m, fs) in &by_method {
            println!("  method={} N={}", m, fs.len());
        }
        if by_method.len() == 2 {
            let (la, a) = &by_method[0];
            let (lb, b) = &by_method[1];
            print_feature("givens", la, &col(a, |f| f.givens as f64), lb, &col(b, |f| f.givens as f64));
            print_feature("trace_len", la, &col(a, |f| f.trace_len as f64), lb, &col(b, |f| f.trace_len as f64));
            print_feature("target_uses", la, &col(a, |f| f.target_count as f64), lb, &col(b, |f| f.target_count as f64));
            print_feature("max_branch", la, &col(a, |f| f.max_branch as f64), lb, &col(b, |f| f.max_branch as f64));
        } else {
            for (m, fs) in &by_method {
                let g = stats(&col(fs, |f| f.givens as f64));
                let tl = stats(&col(fs, |f| f.trace_len as f64));
                let mb = stats(&col(fs, |f| f.max_branch as f64));
                println!("  [{}] givens {:.1}+/-{:.1}  trace_len {:.1}+/-{:.1}  max_branch {:.1}+/-{:.1}", m, g.mean, g.std, tl.mean, tl.std, mb.mean, mb.std);
            }
        }
        for (m, fs) in &by_method {
            let (r, c, bx) = house_split(fs);
            println!("  forced-target house [{}]: row {:.0}% col {:.0}% box {:.0}%", m, pct(r, fs.len()), pct(c, fs.len()), pct(bx, fs.len()));
        }

        // Stuck-state characterization, PER METHOD (so a method's house skew and
        // instance multiplicity are visible — and we can tell a real skew from a
        // "multiple overlapping units, report one" artifact).
        println!("-- stuck state (per method) --");
        for m in &methods {
            let mut walk = Vec::new();
            let mut emp = Vec::new();
            let mut inst = Vec::new();
            let mut agg_hist = [0usize; 10];
            let mut forced_ok = 0;
            let mut total = 0;
            let mut fu_empties: Vec<usize> = Vec::new();
            // House split from the STALL instance (chosen = first), and a
            // multi-kind counter: stalls where >1 unit-KIND hosts a forced quad.
            let (mut hr, mut hc, mut hb) = (0usize, 0usize, 0usize);
            let mut multi_kind = 0;
            let mut multi_inst = 0;
            for r in grp.iter().filter(|r| &r.method == m) {
                let Ok(b) = Board::parse(&r.puzzle) else { continue };
                total += 1;
                let s = stuck_state(&b, &spec, target);
                if s.forced {
                    forced_ok += 1;
                }
                if let Some(fe) = s.forcing_unit_empties {
                    fu_empties.push(fe);
                }
                if s.instances > 1 {
                    multi_inst += 1;
                }
                if s.kinds.len() > 1 {
                    multi_kind += 1;
                }
                match s.kinds.first() {
                    Some(UnitKind::Row) => hr += 1,
                    Some(UnitKind::Col) => hc += 1,
                    Some(UnitKind::Box) => hb += 1,
                    None => {}
                }
                walk.push(s.walk_len as f64);
                emp.push(s.empties as f64);
                inst.push(s.instances as f64);
                for (i, c) in s.cand_hist.iter().enumerate() {
                    agg_hist[i] += c;
                }
            }
            let sw = stats(&walk);
            let se = stats(&emp);
            let si = stats(&inst);
            println!("  [{}] N={} forced {}/{}", m, total, forced_ok, total);
            println!("      walk-before-stall {:.1}+/-{:.1}  empties {:.1}+/-{:.1}  instances {:.2} [{:.0}..{:.0}]", sw.mean, sw.std, se.mean, se.std, si.mean, si.min, si.max);
            println!("      stall-instance house: row {} col {} box {}   (multi-instance stalls {}, multi-KIND stalls {})", hr, hc, hb, multi_inst, multi_kind);
            if !fu_empties.is_empty() {
                let mut counts: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
                for &e in &fu_empties {
                    *counts.entry(e).or_default() += 1;
                }
                let parts: Vec<String> = counts.iter().map(|(k, v)| format!("{}empty={}", k, v)).collect();
                println!("      forcing-unit empties: {}  (quad prediction: all 9)", parts.join(" "));
            }
        }
    }
}
