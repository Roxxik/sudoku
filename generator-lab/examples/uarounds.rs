//! Empirical worst-case round count for the packed UA box-merge min-relaxation.
//!
//! `enumerate_2digit_packed` merges box joins by relaxing `cur = min(cur, cur[pi], cur[pii],
//! cur[na], cur[nb])` for a fixed number of rounds; the result must be FULLY converged
//! (component-min per row) or the build emits non-genuine UA fragments. The number of rounds
//! needed for a pair equals the eccentricity of the component's minimum row in the combined
//! cycle+box graph. This rig replays that exact scalar relaxation to fixpoint over many random
//! solutions and reports the max eccentricity actually seen plus a histogram, so the production
//! `ROUNDS` constant can be set to a value that is both proven-safe and empirically padded.
//!
//! Usage: cargo run --release -p generator-lab --example uarounds -- [boards=2000000] [seed=1]

use generator_lab::fill::random_solution;
use generator_lab::repr::DigitGrid;
use generator_lab::rng::Rng;

fn dense(sol: &DigitGrid) -> [u8; 81] {
    core::array::from_fn(|c| sol.get(c).map_or(0, |d| d.index() as u8))
}

/// Rounds of min-relaxation to reach a fixpoint from `lab` over the four neighbor maps.
fn rounds_to_fixpoint(lab: &[u8; 9], pi: &[u8; 9], pii: &[u8; 9], na: &[u8; 9], nb: &[u8; 9]) -> usize {
    let mut cur = *lab;
    let mut rounds = 0;
    loop {
        let mut next = cur;
        for r in 0..9 {
            let m = cur[r]
                .min(cur[pi[r] as usize])
                .min(cur[pii[r] as usize])
                .min(cur[na[r] as usize])
                .min(cur[nb[r] as usize]);
            next[r] = m;
        }
        if next == cur {
            return rounds;
        }
        cur = next;
        rounds += 1;
    }
}

fn main() {
    let mut it = std::env::args().skip(1);
    let boards: usize = it.next().and_then(|s| s.parse().ok()).unwrap_or(2_000_000);
    let seed: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);

    let mut rng = Rng::from_seed(seed);
    let mut hist = [0u64; 16];
    let mut max_rounds = 0usize;
    let mut worst_board = 0usize;

    for board in 0..boards {
        let g = dense(&random_solution(&mut rng).0);
        // Per-digit coordinate maps (same as the production precompute).
        let mut col_of = [[0u8; 9]; 9]; // [row][digit] = col
        let mut row_in_col = [[0u8; 9]; 9]; // [col][digit] = row
        let mut row_in_box = [[0u8; 9]; 9]; // [box][digit] = row
        for row in 0..9 {
            for col in 0..9 {
                let d = g[row * 9 + col] as usize;
                col_of[row][d] = col as u8;
                row_in_col[col][d] = row as u8;
                row_in_box[(row / 3) * 3 + col / 3][d] = row as u8;
            }
        }
        for a in 0..9usize {
            for b in (a + 1)..9usize {
                // pi(r) = C_b[R_a[r]], pii(r) = C_a[R_b[r]].
                let mut pi = [0u8; 9];
                let mut pii = [0u8; 9];
                let mut na = [0u8; 9];
                let mut nb = [0u8; 9];
                for r in 0..9 {
                    pi[r] = row_in_col[col_of[r][a] as usize][b];
                    pii[r] = row_in_col[col_of[r][b] as usize][a];
                    let band = (r / 3) * 3;
                    na[r] = row_in_box[band + col_of[r][a] as usize / 3][b];
                    nb[r] = row_in_box[band + col_of[r][b] as usize / 3][a];
                }
                // Cycle-min label via min-doubling's fixpoint (scalar): collapse each cycle.
                let mut lab: [u8; 9] = core::array::from_fn(|r| r as u8);
                loop {
                    let mut next = lab;
                    for r in 0..9 {
                        next[r] = lab[r].min(lab[pi[r] as usize]).min(lab[pii[r] as usize]);
                    }
                    if next == lab {
                        break;
                    }
                    lab = next;
                }
                let rd = rounds_to_fixpoint(&lab, &pi, &pii, &na, &nb);
                hist[rd] += 1;
                if rd > max_rounds {
                    max_rounds = rd;
                    worst_board = board;
                }
            }
        }
    }

    println!("boards={boards} seed={seed}: max rounds-to-fixpoint = {max_rounds} (first at board {worst_board})");
    print!("histogram (rounds: count):");
    for (r, &c) in hist.iter().enumerate() {
        if c > 0 {
            print!("  {r}:{c}");
        }
    }
    println!();
}
