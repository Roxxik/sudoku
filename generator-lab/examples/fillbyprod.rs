//! Isolated microbench for the fill byproducts (clue map + UA coordinate inverse): times the
//! OLD three-pass derivation (strip `clue_map` + UA `dense` flatten + coordinate precompute)
//! against the NEW single fused `byproducts` pass the fill hands out, over a pooled solution
//! corpus so the fill and the per-pair UA enumeration (both unchanged) amortize out.
//!
//! Usage: cargo run --release -p generator-lab --example fillbyprod -- [boards=256] [repeats=4000] [seed=1]

use generator_lab::generate::fill_byproduct_cost_pooled;

fn main() {
    let mut it = std::env::args().skip(1);
    let boards: usize = it.next().and_then(|s| s.parse().ok()).unwrap_or(256);
    let repeats: usize = it.next().and_then(|s| s.parse().ok()).unwrap_or(4000);
    let seed: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);

    let (old_ns, new_ns) = fill_byproduct_cost_pooled(seed, boards, repeats);
    let n = (boards * repeats) as f64;
    let (old_pb, new_pb) = (old_ns as f64 / n, new_ns as f64 / n);
    println!(
        "byproducts over {} boards: OLD (clue_map + dense + coord-precompute) {:.1} ns/board, \
         NEW (fused byproducts) {:.1} ns/board, delta {:.1} ns/board ({:+.1}%)",
        n as u64,
        old_pb,
        new_pb,
        old_pb - new_pb,
        (new_pb - old_pb) / old_pb * 100.0
    );
}
