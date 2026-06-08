# 8-wide (ymm) vs 16-wide (zmm) SIMT warp — instruction-shape profile

Companion to `DEEPDIVE.md` (which profiles the production 8-wide warp). This is the
**8 vs 16 lane** comparison: what changes in the *shape of the work* when the unified
warp's SIMD width goes from `Simd<u32,8>` (256-bit ymm) to `Simd<u32,16>` (512-bit zmm).

Focus is the **shape**, not wall-clock throughput (measured on battery; us/att omitted).
Every number below is a count or a ratio — frequency-invariant. The verdict confirms the
source comment at `probe/simt.rs:71`: *"16 / AVX-512 was a wash: Zen 4 double-pumps it."*
Now quantified.

## Method

- `LANES = 8` (production) vs a one-line `LANES = 16` rebuild. `LANES` is the single SIMD
  width const (`V = Simd<u32, LANES>`); flipping it is a genuine zmm warp, verified in
  disasm (below), not oversubscription. Two saved binaries: `/tmp/combobench-l8`, `-l16`.
- **End-to-end, 1:1 macro-lane:slot** (`--lanes == width`, no oversubscription — the user's
  chosen config). Both gates run on the **non-lean** unified warp (`run_warp_unified`,
  `lean=false`, confirmed). Four combo workloads (combobench): HQ=hidden-quad,
  WJ=w-wing+jellyfish, XQ=xyz-wing+naked-quad, SN=swordfish+naked-triple.
- Counts: `perf stat` (boost off, NMI-watchdog off so 6-event groups pin at 100%, pinned
  core 3, small runs — ratios are converged). Instruction shape + register width: Intel
  **SDE `-mix`** (emulated exact dynamic counts; handles every AVX-512 op valgrind chokes
  on; battery-immune). Cross-validated: `sse_avx_ops_retired.all` (HW) = SDE vector-op
  count to <2%.

## The warp is genuinely 16-wide and genuinely full

- `warp_pass_full` disasm: **l8 = 723 ymm + 3 xmm; l16 = 720 zmm + 3 xmm.** Same vector
  *instruction* count per pass, operand width doubled 256->512. Real zmm, not split ymm.
- Lane utilization (`uwstat`, LANES-correct divisor) on all four combo workloads at
  `--lanes 16`: **0.996 (15.93 / 16 lanes active)**. Passes/att **~38.7** vs the ~77 the
  8-wide warp runs — exactly halved, because each pass now services 16 lanes.
- Utilization is *not* the lever. The 16-wide warp is full; the wash is architectural.
  (At `--lanes 8` on the 16-wide build, util is only 7.96/16 — 8 macro-lanes can't fill a
  16-slot warp. Feeding width-many macro-lanes is required and was done.)
- The ~0.4% off 100% is finish-line raggedness: with `--lanes == LANES` there is no
  work-stealing (`next_lane` exhausts), so the first macro-lane to burn its fixed
  per-lane quota leaves its slot idle until the slowest lane finishes. Not a half-fill.

## Whole-program top-down (per attempt; small runs, converged)

| cfg | Kins/att | Kcyc/att | IPC | ret% | fe% | be% | bad-spec% | mispr% | L1 hit% |
|---|---|---|---|---|---|---|---|---|---|
| HQ 8  | 333.3 | 146.4 | 2.28 | 38.4 | 16.8 | 32.5 | 12.0 | 5.47 | 99.39 |
| HQ 16 | 260.8 | 140.0 | 1.86 | 31.8 | 18.1 | **37.3** | 12.7 | 5.55 | 99.08 |
| WJ 8  | 370.9 | 175.9 | 2.11 | 35.2 | 26.1 | 25.8 | 12.6 | 6.40 | 99.35 |
| WJ 16 | 297.1 | 176.2 | 1.69 | 28.3 | 27.3 | **31.1** | 13.0 | 6.83 | 98.60 |
| XQ 8  | 379.2 | 163.3 | 2.32 | 38.9 | 19.6 | 28.7 | 11.9 | 4.73 | 99.19 |
| XQ 16 | 307.5 | 159.8 | 1.92 | 32.5 | 20.3 | **34.1** | 12.8 | 4.92 | 98.83 |
| SN 8  | 317.0 | 140.2 | 2.26 | 38.3 | 18.2 | 30.9 | 12.1 | 6.32 | 99.24 |
| SN 16 | 244.0 | 138.6 | 1.76 | 30.2 | 19.4 | **37.4** | 12.6 | 6.53 | 98.72 |

8->16 per-attempt deltas: **instr/att −19% to −23%**, but **cyc/att −4.4% to +0.2%** (HQ
−4.4, WJ +0.2, XQ −2.1, SN −1.1). IPC falls **2.1–2.3 -> 1.7–1.9**. Backend-stall share
rises everywhere (the binding term gets *worse*). Bad-spec / mispredict-rate flat. L1 hit
rate drops ~0.3–0.8 pp.

## Register-width partition (SDE isa-set; instr/att; sums verified vs `*total`)

| cfg | zmm/512 | ymm/256 | xmm/128 | kmask | scalar/GPR | total |
|---|---|---|---|---|---|---|
| HQ 8  | 2157 | **144986** | 26917 | 2025 | 160687 | 336772 |
| HQ 16 | **76655** | 1141 | 23338 | 1710 | 158096 | 260941 |
| WJ 8  | 2794 | **145695** | 25775 | 2142 | 199597 | 376003 |
| WJ 16 | **77383** | 1269 | 22188 | 1813 | 191958 | 294612 |
| XQ 8  | 2578 | **146627** | 28326 | 2126 | 209182 | 388839 |
| XQ 16 | **77572** | 1341 | 24538 | 1795 | 201685 | 306930 |
| SN 8  | 2165 | **144992** | 26903 | 2036 | 144504 | 320600 |
| SN 16 | **76660** | 1130 | 23310 | 1721 | 141949 | 244769 |

The story in one line: **the ~145K/att of 256-bit ymm warp work collapses to ~1K and is
replaced by ~77K/att of 512-bit zmm** (a clean ~1.9x instruction drop = half the passes x
double width), while **128-bit (fill/strip band ops) and scalar/GPR are width-invariant**
(unchanged per attempt). Vector ops/att: ~176K -> ~103K. Scalar/GPR holds at ~160K/att.

**Scalar share therefore jumps from ~45–54% to ~58–66% of all instructions.** Widening
SIMD attacks only the vector half; the fixed scalar residue (driver refill + per-lane
service + the scalar fish/wing/subset ladder) is the Amdahl ceiling on any width win.

## Top opcodes, HQ 8 vs HQ 16 (per att) — SIMD halves, scalar flat

| opcode | HQ8 | HQ16 | | opcode | HQ8 | HQ16 |
|---|---|---|---|---|---|---|
| MOV       | 50749 | 51079 |  | VPTESTMD   | 13759 | 7495 |
| VPORD     | 28189 | 15529 |  | VPCMPEQD   | 13657 | 6563 |
| VPOPCNTD  | 20126 | 10202 |  | CMP        | 13917 | 13495 |
| VPANDD    | 18317 |  9760 |  | JZ         |  9696 |  9421 |
| VPTERNLOGD| 15626 |  9060 |  | LEA        |  9144 |  8455 |

Every warp-kernel SIMD op (`VPORD/VPOPCNTD/VPANDD/VPTERNLOGD/VPTESTMD/VPCMPEQD/VPSRLD`)
halves per attempt (encoded zmm in l16). Every scalar op (`MOV/CMP/JZ/LEA/PUSH/POP/ADD`)
is essentially identical — the scalar engine does not care about lane width.

## Why it's a wash — the double-pump, directly shown

`sse_avx_ops_retired.all` (retired vector macro-ops): **l8 = 176,730/att, l16 =
105,184/att** — the 16-wide build retires **40% fewer** vector ops, yet **cyc/att is
flat** (146.4K -> 140.0K). If a 512-bit op executed at 256-bit throughput, 40% fewer ops
would buy ~40% fewer vector cycles. It buys ~0. That is the definition of double-pumping:
Zen 4 cracks each 512-bit op into **2 micro-ops over its 256-bit-wide FPU**, so ~half as
many zmm instructions occupy ~the same FP-pipe cycles. The IPC drop (2.1–2.3 -> 1.7–1.9)
and the rising backend-stall share are the same fact seen from the pipeline side.

## Verdict

On Zen 4 (Ryzen 5 7640U), **LANES=16 is a wash: −4.4% to +0.2% cyc/att across the four
rare workloads** — confirming `probe/simt.rs:71`. The wider warp is real and fully
utilized (99.6%); the ceiling is the architecture, not the schedule:

1. **Double-pump** — 512-bit ops are 2x256-bit uops on Zen 4's FPU, so the ~2x fewer zmm
   instructions cost ~the same FP cycles. No raw-throughput gain from width.
2. **Amdahl** — ~50% of per-attempt instructions are width-invariant scalar (driver,
   per-lane service, fish/wing/subset ladder); widening SIMD can't touch them, and they
   become the *majority* (58–66%) at 16-wide.
3. **L1 pressure** — the 16-lane SoA board doubles the resident footprint
   (`[[Simd<u32,16>;3];9]` = 1728 B vs 864 B), nudging L1 hit rate down ~0.3–0.8 pp.

Where width *would* pay: a machine with native 512-bit FPUs (Intel server, Zen 5 has wider
512 datapaths) would not double-pump #1, turning the ~2x instruction reduction into real
cycles for the ~50% that is vector. On this machine, stay at 8 (ymm). The lever remains
what `DEEPDIVE.md` found: cut per-tick vector ops or tick count, and shrink the scalar
residue — not width.
