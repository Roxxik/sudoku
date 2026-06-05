//! Focused profiling target: strip `n` random solutions to minimal with a SINGLE
//! prober variant, so `perf` attributes cleanly (genbench mixes variants and the
//! slow ones swamp the samples). Fill (`random_solution`) and strip both run, so the
//! report separates the fill scan from the strip's `from_digits` rebuild + prober DFS
//! by symbol.
//!
//! Build + profile:
//!   cargo build --release --example proberprof
//!   perf record -g --call-graph dwarf target/release/examples/proberprof 5000
//!   perf report --stdio | head -60
//!
//! The variant under the microscope is fixed below (`Search<Mrv<4>>`); edit + rebuild
//! to profile another.

use generator_lab::fill::random_solution;
use generator_lab::generate::strip_to_minimal;
use generator_lab::probe::Search;
use generator_lab::repr::banded::{Bands, RowMajor};
use generator_lab::rng::Rng;
use generator_lab::scan::Bivalue;

type M = Bands<RowMajor>;
type P = Search<Bivalue>;

fn main() {
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);
    let mut givens = 0usize;
    for seed in 0..n {
        let mut rng = Rng::from_seed(seed);
        let solution = random_solution(&mut rng);
        let puzzle = strip_to_minimal::<M, P>(&mut rng, &solution);
        givens += (0..81).filter(|&i| puzzle.get(i).is_some()).count();
    }
    // Keep the work observable so nothing is optimized away.
    std::hint::black_box(givens);
    println!("{n} seeds, avg givens {:.1}", givens as f64 / n as f64);
}
