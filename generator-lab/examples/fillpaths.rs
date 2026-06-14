//! `fillpaths` — the branch-rule path study for the random full-grid **fill**, the
//! diagnostic twin of `proberpaths` (which studies the uniqueness *prober*). The fill is
//! the first half of every strip attempt: an MRV + random-value DFS that builds a
//! complete solution from the empty board (`crate::fill`). This example re-runs that
//! search under alternative branch-CELL rules and value orders, prices each in the fill's
//! true currencies, and bounds what a better rule could win.
//!
//! Why the prober's answer does NOT transfer. The prober is a `cap = 1` existence search
//! over a near-full board: it has MANY nodes (89% on reverts), and its value order is FREE
//! (verdict-invariant), so reordering children cut passes (-5% via static MCV). The fill
//! is the dual on both axes:
//!   1. Its node count is already at the floor — a complete grid needs >= 81 placements,
//!      and the fill backtracks ~1.7 times/grid, so ~83 nodes/grid is ~98% forced. There
//!      is no "fewer nodes" headroom worth chasing (ceiling ~2-3%).
//!   2. Its value order is PINNED, not free: the order the candidates are tried IS the
//!      sampled output (a different order is a different grid). Derandomizing the value
//!      order to cut backtracks would bias the grid distribution (non-uniform sampling)
//!      and break byte-identical-to-core determinism. So the prober's one realizable lever
//!      is unavailable here.
//! What is left is the per-NODE scan cost (the candidate-count sieve depth), measured in
//! §4 — the fill is scan-bound, and the sieve depth is the one free, byte-identical knob.
//!
//! The instrumented DFS here is a faithful re-implementation of `crate::fill::Fill`: same
//! digit-transposed state, same MRV pick (lowest-index cell of the minimum candidate
//! count), same ascending-then-`rng.shuffle` child order — so for the production rule it
//! consumes the RNG identically and produces byte-identical grids (asserted in §0 against
//! `random_solution_with::<Mrv>`). Re-implemented rather than seamed because the branch
//! study needs to override the cell pick and the value order at every node, which the
//! production `Fill` deliberately does not expose.
//!
//! Usage:
//!   cargo run --release -p generator-lab --example fillpaths -- [attempts] [seed]
//!   # the sieve-depth wall-clock bench (§4) is the realizable measurement:
//!   cargo run --release -p generator-lab --example fillpaths -- 200000 1

use generator_lab::fill::{random_solution, random_solution_with};
use generator_lab::repr::PEERS;
use generator_lab::rng::Rng;
use generator_lab::scan::{LooseMrv, Mrv, MrvRecount};
use std::collections::HashMap;
use std::time::Instant;

const N: usize = 81;

/// Precomputed `1 << cell` and the 20-peer mask per cell — the fill's only board geometry.
struct Geom {
    cell: [u128; N],
    peers: [u128; N],
}
impl Geom {
    fn new() -> Self {
        let mut g = Geom { cell: [0; N], peers: [0; N] };
        for c in 0..N {
            g.cell[c] = 1u128 << c;
            for &p in &PEERS[c] {
                g.peers[c] |= 1u128 << (p as usize);
            }
        }
        g
    }
}

/// Digit-transposed fill state, mirroring `crate::fill::Fill`: `board[d]` = cells where
/// digit `d` may still go, `unsolved` = cells not yet decided, `digits` = the placement
/// shadow. A decided cell's stale bits in other boards are gated out by `unsolved`.
struct Board {
    board: [u128; 9],
    unsolved: u128,
    digits: [u8; N],
}
impl Board {
    fn empty() -> Self {
        Board { board: [(1u128 << N) - 1; 9], unsolved: (1u128 << N) - 1, digits: [0xFF; N] }
    }
    /// Candidate digits of an unsolved `cell` as a 9-bit mask (bit d => digit d may go).
    #[inline]
    fn candidates(&self, cell: usize, g: &Geom) -> u16 {
        let cm = g.cell[cell];
        let mut c = 0u16;
        for d in 0..9 {
            if self.board[d] & cm != 0 {
                c |= 1 << d;
            }
        }
        c
    }
}

/// Which cell a node branches on. `Mrv` is production; the rest are the named alternatives
/// the prober study swept (cell selection), re-asked for the fill.
#[derive(Clone, Copy, PartialEq)]
enum Cell {
    Mrv,     // fewest candidates, ties -> lowest index (production)
    Maxcand, // MOST candidates (anti-MRV) -- the keep-options-open dual
    Lowidx,  // lowest unsolved index, ignore candidate count
    Random,  // a uniformly random unsolved cell
    Bivalue, // a 2-candidate cell if one exists, else lowest unsolved (the prober's rule)
}

/// The order a node tries the chosen cell's candidate digits. `Random` is production
/// (`rng.shuffle`); the deterministic orders are single-sample (the empty board + a fixed
/// policy is one grid), so they report one node count, not a distribution.
#[derive(Clone, Copy, PartialEq)]
enum Val {
    Random,    // ascending-then-shuffle (production)
    Asc,       // ascending digit index
    Desc,      // descending digit index
    Mcv,       // most-constraining: the digit eliminating the most peer candidates first
    Lcv,       // least-constraining first
    Solution,  // follow a precomputed solution (=> 81 nodes, the floor)
}

#[derive(Clone)]
struct Stat {
    nodes: u64,
    reverts: u64,     // a placed digit undone (a child subtree failed)
    grids: u64,
    mincount: [u64; 10], // mincount[k] = branch nodes whose chosen cell had k candidates
    by_unsolved: [u64; N + 1], // nodes bucketed by unsolved-on-entry (board fullness)
    fp: u64,          // xor-fold of produced grids: same rule+stream => same fp
    // Per-grid node cap: a non-MRV cell rule can backtrack explosively (MRV is what keeps
    // the fill near-linear), so a bad rule is bounded to `cap` nodes/grid and recorded as
    // an explosion rather than waited on. `cap == 0` disables it (MRV never explodes).
    cap: u64,
    budget: u64,        // nodes remaining this grid
    grid_capped: bool,  // this grid hit the cap
    capped: u64,        // grids that hit the cap (exploded)
    completed: u64,     // grids that finished under the cap
    completed_nodes: u64, // nodes summed over completed grids only (clean mean)
}
impl Default for Stat {
    fn default() -> Self {
        Stat {
            nodes: 0, reverts: 0, grids: 0, mincount: [0; 10], by_unsolved: [0; N + 1], fp: 0,
            cap: 0, budget: 0, grid_capped: false, capped: 0, completed: 0, completed_nodes: 0,
        }
    }
}

/// One faithful (or rule-overridden) fill from the empty board. Returns the node count of
/// this single grid; accumulates the per-node tallies into `st`. `sol` carries a target
/// completion for `Val::Solution` (ignored otherwise).
fn fill(
    b: &mut Board,
    rng: &mut Rng,
    g: &Geom,
    cellrule: Cell,
    valrule: Val,
    sol: &[u8; N],
    st: &mut Stat,
) -> bool {
    if b.unsolved == 0 {
        return true;
    }
    // Per-grid node cap: a non-MRV cell rule explodes, so bound it and unwind fast (return
    // true so the stack pops without trying more children — the grid is recorded capped).
    if st.cap != 0 {
        if st.budget == 0 {
            st.grid_capped = true;
            return true;
        }
        st.budget -= 1;
    }
    // --- scan: detect dead, pick the branch cell, read its candidates. The min candidate
    // count over unsolved cells is the MRV signal AND the dead test (min == 0 => some cell
    // has no candidate => this branch is dead, exactly the production `unsolved & dead()`).
    let mut min_count = 10usize;
    let mut min_cell = usize::MAX;
    let mut biv = usize::MAX; // lowest 2-candidate cell (for Bivalue)
    let mut max_count = 0usize;
    let mut max_cell = usize::MAX;
    let mut low_cell = usize::MAX;
    let mut nlive = 0u32;
    let mut rest = b.unsolved;
    while rest != 0 {
        let cell = rest.trailing_zeros() as usize;
        rest &= rest - 1;
        let k = b.candidates(cell, g).count_ones() as usize;
        if k == 0 {
            return false; // dead cell: prune without counting a node (matches production)
        }
        nlive += 1;
        if low_cell == usize::MAX {
            low_cell = cell;
        }
        if k < min_count {
            min_count = k;
            min_cell = cell;
        }
        if k > max_count {
            max_count = k;
            max_cell = cell;
        }
        if k == 2 && biv == usize::MAX {
            biv = cell;
        }
    }

    let cell = match cellrule {
        Cell::Mrv => min_cell,
        Cell::Maxcand => max_cell,
        Cell::Lowidx => low_cell,
        Cell::Random => {
            // The j-th live cell, j uniform in [0, nlive). Reuses the production RNG.
            let mut j = rng.range(nlive as usize);
            let mut r = b.unsolved;
            loop {
                let c = r.trailing_zeros() as usize;
                if j == 0 {
                    break c;
                }
                r &= r - 1;
                j -= 1;
            }
        }
        Cell::Bivalue => {
            if biv != usize::MAX {
                biv
            } else {
                low_cell
            }
        }
    };

    st.nodes += 1;
    st.mincount[min_count] += 1;
    st.by_unsolved[nlive as usize] += 1;

    // --- order the chosen cell's candidate digits.
    let cands = b.candidates(cell, g);
    let mut idxs = [0u8; 9];
    let mut n = 0usize;
    let mut m = cands;
    while m != 0 {
        idxs[n] = m.trailing_zeros() as u8;
        m &= m - 1;
        n += 1;
    }
    order_values(&mut idxs[..n], valrule, b, cell, g, sol, rng);

    let cm = g.cell[cell];
    let not_peers = !g.peers[cell];
    b.unsolved &= !cm;
    for i in 0..n {
        let d = idxs[i] as usize;
        let bu = b.board[d];
        b.board[d] &= not_peers;
        b.digits[cell] = idxs[i];
        if fill(b, rng, g, cellrule, valrule, sol, st) {
            return true;
        }
        b.board[d] = bu;
        st.reverts += 1;
    }
    b.unsolved |= cm;
    b.digits[cell] = 0xFF;
    false
}

/// Reorder `idxs` (the cell's candidate digit indices, ascending) per the value rule.
fn order_values(idxs: &mut [u8], rule: Val, b: &Board, cell: usize, g: &Geom, sol: &[u8; N], rng: &mut Rng) {
    match rule {
        Val::Asc => {}
        Val::Random => rng.shuffle(idxs),
        Val::Desc => idxs.reverse(),
        Val::Solution => {
            // Put the solution's digit at this cell first; the rest ascending. Following a
            // consistent solution never backtracks -> exactly 81 nodes (the value oracle).
            let target = sol[cell];
            if let Some(p) = idxs.iter().position(|&x| x == target) {
                idxs[..=p].rotate_right(1);
            }
        }
        Val::Mcv | Val::Lcv => {
            // Constraining-ness of placing digit d at cell = how many unsolved peers still
            // hold d as a candidate (each is eliminated by the placement). MCV tries the
            // most-constraining first, LCV the least.
            let live_peers = g.peers[cell] & b.unsolved;
            let mut key = [0u32; 9];
            for &x in idxs.iter() {
                let d = x as usize;
                key[d] = (b.board[d] & live_peers).count_ones();
            }
            // Stable insertion sort keeps the ascending tiebreak (matches a fixed policy).
            idxs.sort_by(|&a, &c| {
                let (ka, kc) = (key[a as usize], key[c as usize]);
                match rule {
                    Val::Mcv => kc.cmp(&ka).then(a.cmp(&c)),
                    _ => ka.cmp(&kc).then(a.cmp(&c)),
                }
            });
        }
    }
}

/// Run `attempts` fills (seeds `seed..seed+attempts`) under a rule pair, accumulating
/// tallies. For `Val::Solution`/deterministic value orders the RNG is still advanced
/// identically to production for the *cell* `Random` rule, but the produced grid differs.
fn run(attempts: usize, seed: u64, cellrule: Cell, valrule: Val, cap: u64) -> Stat {
    let g = Geom::new();
    let mut st = Stat::default();
    st.cap = cap;
    for s in 0..attempts {
        let mut rng = Rng::from_seed(seed + s as u64);
        // For Val::Solution, first produce a solution with the production rule (own stream)
        // to follow; cheap and keeps the value-oracle honest.
        let sol = if valrule == Val::Solution {
            let mut sb = Board::empty();
            let mut srng = Rng::from_seed(seed + s as u64);
            let mut junk = Stat::default();
            fill(&mut sb, &mut srng, &g, Cell::Mrv, Val::Random, &[0; N], &mut junk);
            sb.digits
        } else {
            [0; N]
        };
        let mut b = Board::empty();
        st.budget = st.cap;
        st.grid_capped = false;
        let nodes_before = st.nodes;
        let ok = fill(&mut b, &mut rng, &g, cellrule, valrule, &sol, &mut st);
        assert!(ok, "fill failed seed {s}");
        st.grids += 1;
        if st.grid_capped {
            st.capped += 1;
        } else {
            st.completed += 1;
            st.completed_nodes += st.nodes - nodes_before;
        }
        let mut h = 0u64;
        for c in 0..N {
            h = h.wrapping_mul(131).wrapping_add(b.digits[c] as u64 + 1);
        }
        st.fp ^= h;
    }
    st
}

/// The 9 cells of box `bx` (row-major within the box).
fn box_cells(bx: usize) -> [usize; 9] {
    let (r0, c0) = ((bx / 3) * 3, (bx % 3) * 3);
    let mut cells = [0usize; 9];
    let mut i = 0;
    for r in r0..r0 + 3 {
        for c in c0..c0 + 3 {
            cells[i] = r * 9 + c;
            i += 1;
        }
    }
    cells
}

/// Place digit `d` at `cell` (an unsolved cell): the production fill's `place` — drop the
/// cell from `unsolved`, forbid `d` on its 20 peers, record the digit.
#[inline]
fn place(b: &mut Board, cell: usize, d: usize, g: &Geom) {
    b.unsolved &= !g.cell[cell];
    b.board[d] &= !g.peers[cell];
    b.digits[cell] = d as u8;
}

/// MRV fill with the first `boxes` DIAGONAL boxes (0, 4, 8) pre-seeded by random
/// permutations — the "prefill cells without branching" idea. The diagonal boxes share no
/// row/column/box, so each is an independent random permutation of 1..9 with zero branching
/// and zero conflict (27 free cells for boxes=3). The remaining cells are then MRV-searched.
/// Returns node + revert tallies over `attempts` seeds (node count == scan count == the
/// fill's wall-clock proxy, since each MRV scan is a fixed 9-board sweep).
fn run_seeded(attempts: usize, seed: u64, boxes: usize) -> Stat {
    let g = Geom::new();
    const DIAG: [usize; 3] = [0, 4, 8];
    let mut st = Stat::default();
    for s in 0..attempts {
        let mut rng = Rng::from_seed(seed + s as u64);
        let mut b = Board::empty();
        for &bx in &DIAG[..boxes] {
            let cells = box_cells(bx);
            let mut digs = [0u8, 1, 2, 3, 4, 5, 6, 7, 8];
            rng.shuffle(&mut digs);
            for (i, &cell) in cells.iter().enumerate() {
                place(&mut b, cell, digs[i] as usize, &g);
            }
        }
        let nodes_before = st.nodes;
        let ok = fill(&mut b, &mut rng, &g, Cell::Mrv, Val::Random, &[0; N], &mut st);
        assert!(ok, "seeded fill failed seed {s} boxes {boxes}");
        st.grids += 1;
        st.completed += 1;
        st.completed_nodes += st.nodes - nodes_before;
    }
    st
}

fn main() {
    let mut args = std::env::args().skip(1);
    let attempts: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(50_000);
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let mode = args.next().unwrap_or_default();

    // `bench` mode runs ONLY the production wall-clock A/B (empty MRV vs diagonal prefill)
    // -- the one thing that needs volume -- so the cheap diagnostics don't tax every timing
    // run. `cargo run ... -- 50000 1 bench`.
    if mode == "bench" {
        let reps = (1_000_000 / attempts.max(1)).max(2);
        println!("production wall-clock: empty vs diagonal prefill ({attempts} grids x {reps} reps):");
        bench_diagonal(attempts, seed, reps);
        return;
    }
    if mode == "depth" {
        // Settled-D=4 confirmation only (not a fresh lever); kept runnable on demand.
        let reps = (1_000_000 / attempts.max(1)).max(2);
        println!("sieve-depth A/B (byte-identical; D=4 is the tuned optimum):");
        bench_depth(attempts, seed, reps);
        return;
    }
    if mode == "entropy" {
        // The concern (now FIXED): `Rng::from_seed` used to load the seed DIRECTLY as the
        // xorshift64 state, and generation feeds SEQUENTIAL seeds. xorshift64 avalanches
        // slowly, so the first outputs from small seeds were skewed -- and the diagonal
        // prefill spends exactly those first draws on the grid's backbone (the 3 box perms),
        // pinning a cell constant (per-cell entropy min 0.000; see docs/FILL-BRANCH-RULE.md
        // §6.5). `from_seed` now SplitMix64-finalizes the seed, so this mode is a REGRESSION
        // CHECK: every config below should now read the full log2(9)=3.170 bits. (The example's
        // own `splitmix` path double-mixes on top of from_seed's mix -- still uniform.)
        //
        // Part A: the RNG stream start itself (no fill) -- the first `range()` decision over
        // sequential seeds, raw vs a SplitMix64-scrambled seed (the candidate fix).
        rng_start_quality(attempts, seed);
        // Part B: does it reach the OUTPUT grid? Per-cell digit entropy (marginal bias; ideal
        // log2(9)=3.170), a non-seeded box's arrangement distinct-rate (joint diversity), and
        // consecutive-seed agreement vs an independent baseline (the correlation smoking gun).
        let n = attempts.min(100_000); // bounded: this generates n grids per config
        println!("\nPart B: output diversity over {n} grids (ideal per-cell entropy {:.3} bits):", 9f64.log2());
        println!(
            "    {:<15} {:>9} {:>9} {:>11} {:>13}",
            "config", "cell_avg", "cell_min", "box1_dist%", "consec/indep",
        );
        for (label, diagonal, scramble) in [
            ("empty raw", false, false),
            ("empty splitmix", false, true),
            ("diagonal raw", true, false),
            ("diagonal splitmix", true, true),
        ] {
            measure_entropy(n, seed, diagonal, scramble, label);
        }
        return;
    }

    // ---- §0. Validate the re-implementation against production -----------------------
    // The instrumented MRV+random fill must produce byte-identical grids to
    // `random_solution_with::<Mrv>` for the same seed (same pick, same RNG stream).
    {
        let g = Geom::new();
        let nval = attempts.min(2000);
        let mut mismatches = 0;
        for s in 0..nval as u64 {
            let mut rng = Rng::from_seed(s);
            let mut b = Board::empty();
            let mut junk = Stat::default();
            fill(&mut b, &mut rng, &g, Cell::Mrv, Val::Random, &[0; N], &mut junk);
            let mut prod_rng = Rng::from_seed(s);
            let prod = random_solution_with::<Mrv>(&mut prod_rng);
            let line = prod.0.to_line();
            let mine: String =
                (0..N).map(|c| (b.digits[c] + 1 + b'0') as char).collect();
            if line != mine {
                mismatches += 1;
            }
        }
        println!(
            "§0 validation: {} / {nval} grids match random_solution_with::<Mrv> {}",
            nval - mismatches,
            if mismatches == 0 { "(OK, re-impl is faithful)" } else { "*** MISMATCH ***" },
        );
    }

    // ---- §1. The workload: where the fill spends its nodes ----------------------------
    let base = run(attempts, seed, Cell::Mrv, Val::Random, 0);
    let grids = base.grids as f64;
    let npg = base.nodes as f64 / grids;
    println!("\n§1 workload (production MRV+random, {attempts} grids seed {seed}):");
    println!(
        "  {:.2} nodes/grid   {:.3} reverts/grid   floor 81  =>  {:.1}% of nodes are forced",
        npg,
        base.reverts as f64 / grids,
        100.0 * 81.0 / npg,
    );
    println!("  branch-cell candidate-count distribution (the MRV min, = the sieve depth lever):");
    let mut cum = 0u64;
    for k in 1..=9 {
        if base.mincount[k] == 0 {
            continue;
        }
        cum += base.mincount[k];
        println!(
            "    {k} cand: {:>5.1}%   (<= {k}: {:>5.1}%)",
            100.0 * base.mincount[k] as f64 / base.nodes as f64,
            100.0 * cum as f64 / base.nodes as f64,
        );
    }
    // Naked-single fraction: the cells MRV picks at count 1 (the depth-2 fast-path target).
    println!(
        "  naked singles (min==1): {:.1}% of nodes  <- depth-2 fast-path candidate",
        100.0 * base.mincount[1] as f64 / base.nodes as f64,
    );
    println!("  nodes by board fullness (unsolved cells on entry):");
    for (lo, hi) in [(0, 15), (16, 31), (32, 47), (48, 63), (64, 81)] {
        let s: u64 = base.by_unsolved[lo..=hi.min(N)].iter().sum();
        println!("    {lo:>2}-{hi:<2} unsolved: {:>5.1}%", 100.0 * s as f64 / base.nodes as f64);
    }

    // ---- §2. Branch-CELL selection -----------------------------------------------------
    // Random value order (production), vary the cell rule. MRV is what keeps the empty-board
    // fill near-linear: a non-MRV rule backtracks explosively, so each grid is capped and an
    // exploded grid is reported as a cap-hit, not waited on. The mean is over grids that
    // FINISHED under the cap (`nodes/grid (done)`); `capped%` is the explosion rate.
    // A bad rule only has to SHOW it explodes (capped%), not be measured precisely, so a
    // small sample + low cap is plenty; the good rules finish in ~85 nodes and never cap.
    let cap = 8_000u64;
    let s2 = attempts.min(400);
    println!("\n§2 branch-CELL rule (random value order, {s2} grids, cap {cap} nodes/grid):");
    println!("    {:<10} {:>14} {:>9} {:>8}", "rule", "nodes/grid(done)", "vs MRV", "capped%");
    for (name, rule) in [
        ("mrv", Cell::Mrv),
        ("bivalue", Cell::Bivalue),
        ("lowidx", Cell::Lowidx),
        ("random", Cell::Random),
        ("maxcand", Cell::Maxcand),
    ] {
        let st = run(s2, seed, rule, Val::Random, cap);
        let done = st.completed.max(1);
        let n = st.completed_nodes as f64 / done as f64;
        let capped_pct = 100.0 * st.capped as f64 / st.grids as f64;
        let vs = if st.completed > 0 {
            format!("{:>+7.1}%", 100.0 * (n / npg - 1.0))
        } else {
            "  n/a".to_string()
        };
        println!("    {:<10} {:>14.2} {:>9} {:>7.1}%", name, n, vs, capped_pct);
    }

    // ---- §3. Value ordering (the pinned axis) -----------------------------------------
    // MRV cell held; vary value order. Deterministic orders are single-sample (one grid
    // each from the empty board); random is the seed-set mean. `solution` = the floor.
    println!("\n§3 VALUE order (MRV cell). Deterministic orders are 1 grid; random is the mean:");
    println!("    {:<11} {:>10} {:>9}", "order", "nodes/grid", "vs floor");
    for (name, rule, single) in [
        ("solution", Val::Solution, false), // value oracle: == 81
        ("ascending", Val::Asc, true),
        ("descending", Val::Desc, true),
        ("mcv", Val::Mcv, true),
        ("lcv", Val::Lcv, true),
        ("random(prod)", Val::Random, false),
    ] {
        // Single-sample deterministic orders: one grid (seed `seed`). Random/solution: mean.
        let st = if single {
            run(1, seed, Cell::Mrv, rule, 0)
        } else {
            run(attempts, seed, Cell::Mrv, rule, 0)
        };
        let n = st.nodes as f64 / st.grids as f64;
        println!("    {:<11} {:>10.2} {:>8.1}%", name, n, 100.0 * (n / 81.0 - 1.0));
    }
    println!("  (deterministic value orders need ONE try -> a fixed grid distribution =");
    println!("   non-uniform sampling, and break byte-identical-to-core: the axis is closed.)");

    // ---- §5. Diagonal-box prefill: cut SCANS, not branches ----------------------------
    // Each MRV scan is a fixed 9-board sweep, so node count == scan count == the fill's
    // wall-clock proxy. The 3 diagonal boxes (0/4/8) share no unit, so 9*boxes cells fill
    // for free (one random permutation each, zero branching, zero conflict) and never get
    // scanned. The question is whether completing the REST backtracks more (a random
    // diagonal always extends, but the search is not forced). nodes/grid is the headline:
    // boxes=3 removes 27 cells, so the floor it could reach is ~82-27 = 55 nodes.
    println!("\n§5 diagonal-box prefill (MRV completes the rest, {attempts} grids):");
    println!("    {:<14} {:>10} {:>12} {:>9}", "prefilled", "nodes/grid", "reverts/grid", "vs empty");
    let empty_n = base.nodes as f64 / base.grids as f64;
    for boxes in 0..=3 {
        let st = run_seeded(attempts, seed, boxes);
        let n = st.nodes as f64 / st.grids as f64;
        let label = if boxes == 0 {
            "none (empty)".to_string()
        } else {
            format!("{boxes} box ({} cells)", boxes * 9)
        };
        println!(
            "    {:<14} {:>10.2} {:>12.3} {:>+8.1}%",
            label,
            n,
            st.reverts as f64 / st.grids as f64,
            100.0 * (n / empty_n - 1.0),
        );
    }
    println!("  (node==scan==wall-clock proxy; a real fill must seed via the production rep");
    println!("   and changes the grid stream, so it is NOT byte-identical to core.)");
    println!("\n  (wall-clock payoff: run with a `bench` 3rd arg, e.g. `-- 50000 1 bench`.");
    println!("   sieve-depth A/B is a separate `depth` mode -- it is settled at D=4, not a lever.)");
}

/// The §5 payoff in wall-clock: time the production empty-board MRV fill against the
/// diagonal-seeded MRV fill, both on the real banded rep. Same warm-up + reps discipline as
/// `bench_depth`. The fps differ (diagonal explores a different stream) -- that is the
/// not-byte-identical cost, reported, not hidden.
fn bench_diagonal(attempts: usize, seed: u64, reps: usize) {
    let timed = |empty: bool| -> (f64, u64) {
        let mut fp = 0u64;
        for s in 0..attempts as u64 {
            let mut rng = Rng::from_seed(seed + s);
            let sol = if empty {
                random_solution_with::<Mrv>(&mut rng)
            } else {
                random_solution(&mut rng)
            };
            fp ^= fold(&sol.0.to_line());
        }
        let t = Instant::now();
        let mut sink = 0u64;
        for _ in 0..reps {
            for s in 0..attempts as u64 {
                let mut rng = Rng::from_seed(seed + s);
                let sol = if empty {
                    random_solution_with::<Mrv>(&mut rng)
                } else {
                    random_solution(&mut rng)
                };
                sink ^= sol.0.get(40).map_or(0, |d| d.index() as u64 + 1);
            }
        }
        let per = t.elapsed().as_secs_f64() * 1e9 / (reps * attempts) as f64;
        let _ = sink;
        (per, fp)
    };
    // One warm-up pass folded into `timed` (its own warm loop), then one timed pass each.
    let (empty_ns, efp) = timed(true);
    let (diag_ns, dfp) = timed(false);
    println!("    empty MRV (baseline) {empty_ns:>7.1} ns/grid   fp={efp:#018x}");
    println!("    diagonal (now prod)  {diag_ns:>7.1} ns/grid   fp={dfp:#018x}  (different grid stream)");
    println!(
        "    => diagonal is {:.2}x  ({:+.1}% wall, vs the -32.6% scan-count projection)",
        empty_ns / diag_ns,
        100.0 * (diag_ns / empty_ns - 1.0),
    );
}

/// Time `random_solution_with::<S>` for each sieve-depth / over-cap policy, reporting
/// ns/grid and a grid fingerprint (identical fp => the change was free / byte-identical).
fn bench_depth(attempts: usize, seed: u64, reps: usize) {
    macro_rules! bench {
        ($label:literal, $strat:ty) => {{
            // Warm up, then time `reps` passes over the seed set; keep a sink so the fill
            // is not optimized away, and fold a fingerprint to prove byte-identity.
            let mut fp = 0u64;
            for s in 0..attempts as u64 {
                let mut rng = Rng::from_seed(seed + s);
                let sol = random_solution_with::<$strat>(&mut rng);
                fp ^= fold(&sol.0.to_line());
            }
            let t = Instant::now();
            let mut sink = 0u64;
            for _ in 0..reps {
                for s in 0..attempts as u64 {
                    let mut rng = Rng::from_seed(seed + s);
                    let sol = random_solution_with::<$strat>(&mut rng);
                    // Cheap sink (one cell read, no String alloc) so the loop times the FILL,
                    // not a `to_line` allocation; still defeats dead-code elimination.
                    sink ^= sol.0.get(40).map_or(0, |d| d.index() as u64 + 1);
                }
            }
            let dt = t.elapsed().as_secs_f64();
            let per = dt * 1e9 / (reps * attempts) as f64;
            println!(
                "    {:<16} {:>7.1} ns/grid   fp={:#018x}  (sink {})",
                $label, per, fp, sink & 1,
            );
        }};
    }
    // Same pick across depths -> identical fp; ns/grid is the only thing that moves.
    bench!("mrv<2>", Mrv<2>);
    bench!("mrv<3>", Mrv<3>);
    bench!("mrv<4> (prod)", Mrv<4>);
    bench!("mrv<5>", Mrv<5>);
    bench!("mrv<6>", Mrv<6>);
    bench!("mrv<9>", Mrv<9>);
    // Over-cap policy A/Bs at production depth. MrvRecount picks the same cell (byte-
    // identical); LooseMrv gives up MRV over-cap (a DIFFERENT grid -> different fp).
    bench!("recount<4>", MrvRecount<4>);
    bench!("loose<4>", LooseMrv<4>);
    bench!("loose<2>", LooseMrv<2>);
}

fn fold(line: &str) -> u64 {
    let mut h = 0u64;
    for byte in line.bytes() {
        h = h.wrapping_mul(131).wrapping_add(byte as u64);
    }
    h
}

/// SplitMix64 finalizer: turns a sequential counter seed into a well-mixed 64-bit state, so
/// consecutive seeds decorrelate before the first draw. The candidate fix for the direct
/// `from_seed` load. (`from_seed(0)` special-cases 0; splitmix never outputs 0 for these.)
fn splitmix(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// Part A: the RNG stream start. For sequential seeds, the FIRST shuffle decision is
/// `range(9)` (a 9-element Fisher-Yates starts `range(9)`), which is also the diagonal
/// prefill's very first draw (box 0's first swap). Tally its distribution over `n` sequential
/// seeds, raw seed vs SplitMix64-scrambled; a skewed/low-entropy raw histogram is the flaw.
fn rng_start_quality(n: usize, base: u64) {
    println!("Part A: first range(9) over {n} sequential seeds (ideal: uniform, {:.3} bits):", 9f64.log2());
    for (label, scram) in [("raw seed", false), ("splitmix", true)] {
        let mut h = [0u64; 9];
        for s in base..base + n as u64 {
            let seed = if scram { splitmix(s) } else { s };
            let mut r = Rng::from_seed(seed);
            h[r.range(9)] += 1;
        }
        let ent = entropy(&h);
        let top = h.iter().copied().max().unwrap_or(0);
        let topbin = h.iter().position(|&c| c == top).unwrap_or(0);
        println!(
            "    {:<9} entropy {ent:.3} bits   top bin = {topbin} at {:.1}%   dist {:?}",
            label,
            100.0 * top as f64 / n as f64,
            h.iter().map(|&c| (1000.0 * c as f64 / n as f64).round() / 10.0).collect::<Vec<_>>(),
        );
    }
}

/// Shannon entropy (bits) of a count histogram.
fn entropy(counts: &[u64]) -> f64 {
    let tot: u64 = counts.iter().sum();
    if tot == 0 {
        return 0.0;
    }
    let mut h = 0.0;
    for &c in counts {
        if c > 0 {
            let p = c as f64 / tot as f64;
            h -= p * p.log2();
        }
    }
    h
}

/// Generate one grid's digits from seed `s`, optionally diagonal-seeded and/or splitmix-
/// scrambled, as a dense `[digit; 81]`.
fn gen_grid(s: u64, diagonal: bool, scramble: bool) -> [u8; N] {
    let mut rng = Rng::from_seed(if scramble { splitmix(s) } else { s });
    let sol = if diagonal {
        random_solution(&mut rng)
    } else {
        random_solution_with::<Mrv>(&mut rng)
    };
    core::array::from_fn(|c| sol.0.get(c).map_or(0, |d| d.index() as u8))
}

/// Part B: per-config output diversity. `cell_avg`/`cell_min` = mean/min per-cell digit
/// entropy (marginal bias). `box1_dist%` = distinct arrangements of box 1 (the top-middle,
/// NON-seeded box) as a fraction of grids (joint diversity of the completion). `consec/indep`
/// = mean matching-cells between consecutive-seed grids over the independent-pair baseline; a
/// ratio > 1 means sequential seeds produce correlated grids (the from_seed-load flaw).
fn measure_entropy(n: usize, base: u64, diagonal: bool, scramble: bool, label: &str) {
    const BOX1: [usize; 9] = [3, 4, 5, 12, 13, 14, 21, 22, 23];
    const FAR: u64 = 1_000_003; // a large, coprime-ish offset for the independent baseline
    let mut cell = [[0u64; 9]; N];
    let mut box1: HashMap<u32, u32> = HashMap::new();
    let mut prev: Option<[u8; N]> = None;
    let (mut consec, mut consec_pairs) = (0u64, 0u64);
    let (mut indep, mut indep_pairs) = (0u64, 0u64);
    let matches = |a: &[u8; N], b: &[u8; N]| (0..N).filter(|&c| a[c] == b[c]).count() as u64;
    for i in 0..n {
        let s = base + i as u64;
        let g = gen_grid(s, diagonal, scramble);
        for c in 0..N {
            cell[c][g[c] as usize] += 1;
        }
        let mut key = 0u32;
        for &cc in &BOX1 {
            key = key * 9 + g[cc] as u32;
        }
        *box1.entry(key).or_insert(0) += 1;
        if let Some(p) = prev {
            consec += matches(&p, &g);
            consec_pairs += 1;
        }
        prev = Some(g);
        // Independent baseline on a subset (pair s with s+FAR): the expected agreement for
        // genuinely uncorrelated seeds, under the SAME fill, so the ratio isolates the seed.
        if i < n / 5 {
            let g2 = gen_grid(s + FAR, diagonal, scramble);
            indep += matches(&g, &g2);
            indep_pairs += 1;
        }
    }
    let cell_ent: Vec<f64> = (0..N).map(|c| entropy(&cell[c])).collect();
    let cell_avg = cell_ent.iter().sum::<f64>() / N as f64;
    let cell_min = cell_ent.iter().cloned().fold(f64::INFINITY, f64::min);
    let box1_dist = 100.0 * box1.len() as f64 / n as f64;
    let consec_m = consec as f64 / consec_pairs.max(1) as f64;
    let indep_m = indep as f64 / indep_pairs.max(1) as f64;
    println!(
        "    {:<15} {:>9.4} {:>9.4} {:>10.1}% {:>9.2} ({:.2}/{:.2})",
        label,
        cell_avg,
        cell_min,
        box1_dist,
        consec_m / indep_m.max(1e-9),
        consec_m,
        indep_m,
    );
}
