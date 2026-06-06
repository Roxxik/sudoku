//! Isolated prober bench: pack's scalar uniqueness prober (one query at a time,
//! the shape the sequential generator calls) vs the packed `PackedProber` (W=8
//! refill warp) on the SAME real query stream, per core. This isolates the
//! prober-kernel speedup from the Amdahl dilution `packbench` sees end-to-end
//! (where the still-scalar baseline gate and grid fill cap the win).
//!
//! It mirrors generator-lab's `killswitch` example, but measures pack's *actual
//! deployed* probers: the scalar `Search<Bivalue>` existence prober (the scalar bar,
//! one query at a time) against `PackedProber::resolve` (the new smear+ALU
//! gather-free kernel). Both verdicts are cross-checked against each other and
//! against the live prober (they MUST all agree — existence is deterministic), so
//! this is a go/no-go on the kernel, not a guess.
//!
//! Usage: cargo run --release -p generator-pack --example probebench -- [--attempts N=2000] [--iters I=30] [--mode train|drill]

use generator_lab::bb::{BitBoard, Placed};
use generator_lab::fill::random_full_grid;
use generator_lab::grid::{CELLS, Digit, digit_to_bit};
use generator_lab::probe::{Prober, Search};
use generator_lab::repr::banded::{Bands, RowMajor};
use generator_lab::repr::{DigitGrid, Marks, SearchState};
use generator_lab::rng::Rng;
use generator_lab::scan::Bivalue;
use generator_lab::simt::prober::{PackedProber, Probe};
use generator_lab::spec_for_mode;
use std::time::Instant;

/// The banded packing the scalar prober branches on.
type RM = Bands<RowMajor>;
/// The scalar prober: scan/sieve `Search` with the `Bivalue` branch strategy — the
/// shape the sequential generator calls, one existence query at a time.
type P = Search<Bivalue>;

/// The restricted prober state for the strip of clue `orig` at `i`: a `SearchState`
/// built from the cleared `cells` with `orig` forbidden at `i`, so an existence probe
/// asks whether some *alternate* digit still completes (bb's old alt-completion probe).
/// This is the scalar oracle and the scalar timing bar for the packed prober.
fn restricted_state(cells: &[Digit; CELLS], i: usize, orig: Digit) -> SearchState<RM> {
    let grid = DigitGrid::from_array(core::array::from_fn(|c| {
        generator_lab::repr::Digit::new(cells[c])
    }));
    let mut state = SearchState::<RM>::from_digits(&grid);
    state.forbid(i, generator_lab::repr::Digit::new(orig).expect("nonzero clue digit"));
    state
}

fn main() {
    let mut attempts = 2000usize;
    let mut iters = 30usize;
    let mut mode = 0u32;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            "--mode" => mode = if it.next().as_deref() == Some("drill") { 1 } else { 0 },
            _ => {}
        }
    }

    // ---- collect the real query stream from the strip loop ----
    // At every uniqueness gate we snapshot both forms of the same query: a clone of
    // the live board + (cell, alts) for the scalar prober, and the exported
    // row-major bands for the packed prober. The strip then advances exactly as the
    // generator would (revert non-unique or baseline-unsolved strips) so the stream
    // is the genuine workload, not a synthetic one.
    let spec = spec_for_mode(mode);
    let baseline = spec.baseline_mask();
    let forced = spec.forced_mask();
    let mut rng = Rng::from_seed(1);
    let mut probes: Vec<Probe> = Vec::new();
    // The restricted scalar prober state per query (orig forbidden at its cell): the
    // scalar oracle/bar, the new-stack twin of bb's (board, cell, alts) probe.
    let mut states: Vec<SearchState<RM>> = Vec::new();
    let mut reals: Vec<bool> = Vec::new();

    for _ in 0..attempts {
        let solution = random_full_grid(&mut rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        let mut bb = BitBoard::from_board(&solution);
        let mut placed = Placed::from_board(&solution);
        let mut cells: [Digit; CELLS] = core::array::from_fn(|i| solution.cell(i));
        for i in positions {
            if cells[i] == 0 {
                continue;
            }
            let orig = cells[i];
            cells[i] = 0;
            let cand = bb.apply_clear(i, orig, &mut placed);
            let alts = cand & !digit_to_bit(orig);
            if alts == 0 {
                continue;
            }
            let (r, unsolved) = bb.export_r();
            probes.push(Probe { r, unsolved, cell: i, alts });
            let state = restricted_state(&cells, i, orig);
            let real = P::has_completion(state.clone());
            states.push(state);
            reals.push(real);
            if real {
                cells[i] = orig;
                bb.apply_place(i, orig, &mut placed);
                continue;
            }
            let o = bb.baseline(baseline, forced);
            if !o.solved {
                cells[i] = orig;
                bb.apply_place(i, orig, &mut placed);
            }
        }
    }
    let n = probes.len();

    // ---- soundness: scalar vs live, packed vs live, packed vs scalar ----
    let scalar_verdicts: Vec<bool> = states.iter().map(|s| P::has_completion(s.clone())).collect();
    let mut packed_verdicts = vec![false; n];
    let mut prober = PackedProber::new();
    prober.resolve(&probes, &mut packed_verdicts);

    let mut scalar_vs_real = 0u64;
    let mut packed_vs_real = 0u64;
    let mut packed_vs_scalar = 0u64;
    let mut nonunique = 0u64;
    for q in 0..n {
        scalar_vs_real += (scalar_verdicts[q] != reals[q]) as u64;
        packed_vs_real += (packed_verdicts[q] != reals[q]) as u64;
        packed_vs_scalar += (packed_verdicts[q] != scalar_verdicts[q]) as u64;
        nonunique += reals[q] as u64;
    }

    let mode_name = if mode == 1 { "drill" } else { "train" };
    println!("probebench: mode={mode_name}, W={}, {n} queries  (non-unique {:.1}%)", generator_lab::simt::prober::LANES, 100.0 * nonunique as f64 / n as f64);
    println!("  verdict mismatches  scalar-vs-live {scalar_vs_real}  packed-vs-live {packed_vs_real}  packed-vs-scalar {packed_vs_scalar}   <- all MUST be 0\n");

    // ---- timing ----
    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for s in &states {
            acc = acc.wrapping_add(P::has_completion(s.clone()) as u64);
        }
    }
    let ns_s = t.elapsed().as_secs_f64() * 1e9 / (n * iters) as f64;
    std::hint::black_box(acc);

    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        prober.resolve(&probes, &mut packed_verdicts);
        acc = acc.wrapping_add(packed_verdicts.iter().filter(|&&v| v).count() as u64);
    }
    let ns_v = t.elapsed().as_secs_f64() * 1e9 / (n * iters) as f64;
    std::hint::black_box(acc);

    println!("  scalar (one query/call)  {ns_s:>8.2} ns/query");
    println!("  packed (W=8 refill warp) {ns_v:>8.2} ns/query");
    println!("\n  PER-CORE prober speedup (scalar / packed): {:.2}x", ns_s / ns_v);
}
