//! Single-path profiling target for the NEW-repr generator: run `generate::run_attempts`
//! on one spec in a tight loop so `perf` attributes cleanly (construction vs solve vs
//! prober by symbol).
//!   cargo build --release -p generator-lab --example genprof
//!   perf record -g --call-graph dwarf target/release/examples/genprof --mode drill --attempts 12000
//!   perf report --stdio | head -50
use generator_lab::generate;
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;
fn main() {
    let mut attempts = 12000usize;
    let mut mode = 0u32;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
            "--mode" => mode = if it.next().as_deref() == Some("drill") { 1 } else { 0 },
            _ => {}
        }
    }
    let spec = spec_for_mode(mode);
    let (s, fp) = generate::run_attempts(&mut Rng::from_seed(1), &spec, attempts);
    let succ = s.successes;
    println!("new mode={mode} attempts={attempts} succ={succ} fp={fp:#018x}");
}
