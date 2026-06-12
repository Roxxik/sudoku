#!/usr/bin/env bash
#
# deepdive-repro.sh — reproduce the QUANTITATIVE numbers in DEEPDIVE.md,
# DEEPDIVE-LOG.md and DEEPDIVE-LANES16.md in one run, using the CURRENT code
# (symbol/host names drift; the perf events and findpar-bench interface do not).
#
# WHAT THIS SCRIPT DOES (the deterministic, measurable half):
#   topdown   §2 table + DEEPDIVE-LOG facts: us/att, cyc/att, instr/att, IPC,
#             the five Zen-4 slot buckets (retiring/frontend/backend/bad-spec/smt),
#             MPKI, taken%, mispredict-rate, op-cache hit% + %ops-from-opcache,
#             L1d loads/att + miss%, vector-ops/att, divides/att.
#   util      §3: unified-warp lane utilization + warp passes/att (findpar-bench
#             --features count prints these directly).
#   selftime  §4: per-symbol self-time for the named hot regions (perf record/report).
#   lanes16   DEEPDIVE-LANES16: l8-vs-l16 top-down + double-pump (sse_avx_ops/att,
#             cyc/att flat) + objdump ymm/zmm/xmm operand counts in warp_pass_full.
#   sde       DEEPDIVE-LANES16: SDE -mix register-width partition + top-opcode table.
#
# WHAT THIS SCRIPT DELIBERATELY DOES NOT DO (needs an agent or a human — judgment,
# not measurement). Printed again at the end as a checklist:
#   - §5 instruction-level dossiers: reading disasm and deciding a hot line *is* the
#     CellMarks::get bounds check, or that the ~26% vzeroupper / ~34% kmovd samples
#     are pipeline SKIDS onto a call/serialization boundary, not real cost.
#   - The BINDING-TERM verdict per region (which stall bucket actually binds) and the
#     §2 cost model.
#   - Mapping per-symbol self-time onto NAMED regions when they are inlined
#     (clear_clue has no standalone symbol) or aggregated (the "subset family").
#   - §6 memory reconciliation and §7 optimization candidates + risk.
#
# Usage:
#   docs/deepdive-repro.sh [--quick] [--sudo] [SECTION ...]
#     SECTION ∈ {topdown util selftime lanes16 sde all}   (default: all)
#     --quick   tiny attempt counts (smoke the plumbing, NOT publishable numbers)
#     --sudo    let the script set the canonical perf env (paranoid/watchdog/boost/
#               governor) via sudo; DEFAULT is to only PRINT the commands to run
#
# Faithful absolute numbers (us/att, cyc/att) need the DEEPDIVE measurement env:
# governor=performance, boost OFF, perf_event_paranoid=-1, nmi_watchdog=0, pinned to
# one core. By default the script only reports the env and prints the commands to fix
# it; pass --sudo to have it set them for you (best-effort, non-interactive).
# The RATIOS (top-down %, IPC, MPKI, per-att counts, utilization) are frequency-
# invariant and reproduce even with boost on.

set -euo pipefail

# ───────────────────────── config ─────────────────────────
CORE=3                       # taskset pin (DEEPDIVE used core 3)
PER_LANE=10000               # 8 lanes × 10000 = 80000 attempts (DEEPDIVE study size)
PER_LANE_SDE=120             # SDE emulates ~10-50×; per-att counts converge tiny
SEED=1
LANES=8

# Four study workloads: LABEL = comma-separated --force kinds (domain names, stable).
WORKLOADS=(
  "HQ|hidden-quad"
  "WJ|w-wing,jellyfish"
  "XQ|xyz-wing,naked-quad"
  "SN|swordfish,naked-triple"
)

# §4 self-time regions: LABEL = substring matched (anywhere) against demangled perf
# symbols; self-% is summed over all matches (e.g. generic monomorphs). Edit if a
# symbol is renamed. branch_cell is now inlined into branch_lane (expect ~0 — that
# zero is itself the finding). The driver regions are the post-warp_host names.
SELFTIME_SYMS=(
  "warp_pass_full|warp_pass_full"
  "subset_step|subset_step"
  "fish_step|fish_step"
  "wing_step|wing_step"
  "fill|::fill::Fill"
  "clear_clue|clear_clue"
  "branch_lane|branch_lane"
  "branch_cell|branch_cell"
  "backtrack_lane|backtrack_lane"
  "driver_advance|>>::advance"
  "driver_pump|>>::pump"
)

# Zen-4 perf event groups (≤6 each so they pin when nmi_watchdog=0).
G_TOPDOWN="cycles,ex_ret_ops,de_src_op_disp.all,de_no_dispatch_per_slot.backend_stalls,de_no_dispatch_per_slot.no_ops_from_frontend,de_no_dispatch_per_slot.smt_contention"
G_BRANCH="cycles,instructions,ex_ret_brn,ex_ret_brn_misp,ex_ret_brn_tkn,ex_div_count"
G_OPCACHE="cycles,op_cache_hit_miss.op_cache_hit,op_cache_hit_miss.all_op_cache_accesses,de_src_op_disp.op_cache,de_src_op_disp.all,sse_avx_ops_retired.all"
G_L1="cycles,instructions,ls_dispatch.ld_dispatch,all_data_cache_accesses,l2_cache_req_stat.dc_access_in_l2,ls_dispatch.store_dispatch"

# ───────────────────────── locate crate / paths ─────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"          # generator-lab/
[[ -f "$CRATE_DIR/Cargo.toml" ]] || { echo "cannot find generator-lab Cargo.toml from $SCRIPT_DIR" >&2; exit 1; }
TARGET_DIR="$(cd "$CRATE_DIR/.." && pwd)/target/release/examples"
WORK="$(mktemp -d)"                                # /tmp is shared: use a private dir
SDE="${SDE:-$HOME/opt/sde-external-10.8.0-2026-03-15-lin/sde64}"
LANES_FILE="$CRATE_DIR/src/probe/simt.rs"

# ───────────────────────── args ─────────────────────────
QUICK=0; USE_SUDO=0; SECTIONS=()
for a in "$@"; do
  case "$a" in
    --quick)   QUICK=1 ;;
    --sudo)    USE_SUDO=1 ;;
    topdown|util|selftime|lanes16|sde) SECTIONS+=("$a") ;;
    all)       SECTIONS=(topdown util selftime lanes16 sde) ;;
    *) echo "unknown arg: $a" >&2; exit 2 ;;
  esac
done
[[ ${#SECTIONS[@]} -eq 0 ]] && SECTIONS=(topdown util selftime lanes16 sde)
if [[ $QUICK -eq 1 ]]; then PER_LANE=1000; PER_LANE_SDE=40; fi

rule(){ printf '%s\n' "────────────────────────────────────────────────────────────────────────"; }
say(){  printf '%s\n' "$*"; }
have(){ command -v "$1" >/dev/null 2>&1; }

# ───────────────────────── canonical perf environment ─────────────────────────
# Each setting needs root. Try `sudo -n` (non-interactive); if it fails, collect the
# command so the user can run it via `! <cmd>` and continue (ratios stay valid).
NEED_CMDS=()
trysudo(){ # path value human
  local path="$1" want="$2" name="$3" cur
  cur="$(cat "$path" 2>/dev/null || echo '?')"
  [[ "$cur" == "$want" ]] && { say "  $name: $cur (ok)"; return; }
  if [[ $USE_SUDO -eq 1 ]] && sudo -n sh -c "echo $want > $path" 2>/dev/null; then
    say "  $name: $cur -> $want (set)"
  else
    say "  $name: $cur (want $want) -- NOT set"
    NEED_CMDS+=("echo $want | sudo tee $path")
  fi
}
setup_env(){
  rule; say "ENVIRONMENT (DEEPDIVE: governor=performance, boost OFF, paranoid=-1, watchdog=0)"
  trysudo /proc/sys/kernel/perf_event_paranoid -1 "perf_event_paranoid"
  trysudo /proc/sys/kernel/nmi_watchdog 0 "nmi_watchdog (frees the 6th PMC so groups pin)"
  trysudo /sys/devices/system/cpu/cpufreq/boost 0 "cpufreq boost (OFF = stable cyc<->time)"
  # governor: set on the pinned core (and siblings cheaply)
  local g="/sys/devices/system/cpu/cpu$CORE/cpufreq/scaling_governor" cur
  cur="$(cat "$g" 2>/dev/null || echo '?')"
  if [[ "$cur" == performance ]]; then say "  governor(cpu$CORE): performance (ok)"
  elif [[ $USE_SUDO -eq 1 ]] && sudo -n sh -c "echo performance > $g" 2>/dev/null; then
    say "  governor(cpu$CORE): $cur -> performance (set)"
  else say "  governor(cpu$CORE): $cur (want performance) -- NOT set"
    NEED_CMDS+=("echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor"); fi
  if [[ "$(cat /sys/devices/system/cpu/cpufreq/boost 2>/dev/null)" == 1 ]]; then
    say "  NOTE: boost is ON -> absolute us/att & cyc/att run fast and noisy; RATIOS still valid."
  fi
  if [[ ${#NEED_CMDS[@]} -gt 0 ]]; then
    say ""
    if [[ $USE_SUDO -eq 1 ]]; then
      say "  Could not set the above via sudo -n. Run these yourself (e.g. via '! <cmd>'):"
    else
      say "  Not in canonical env (sudo opt-in: pass --sudo to let the script set these,"
      say "  or run them yourself, e.g. via '! <cmd>'):"
    fi
    printf '    %s\n' "${NEED_CMDS[@]}"
  fi
}

# ───────────────────────── build helpers ─────────────────────────
build_findpar_bench(){ # features-or-empty  out-suffix
  local feats="$1" suff="$2" flags=()
  [[ -n "$feats" ]] && flags=(--features "$feats")
  ( cd "$CRATE_DIR" && cargo build --release "${flags[@]}" --example findpar-bench >/dev/null 2>&1 )
  cp "$TARGET_DIR/findpar-bench" "$TARGET_DIR/findpar-bench-$suff"
  echo "$TARGET_DIR/findpar-bench-$suff"
}

# Run findpar-bench (prints to $WORK/out) under a perf-stat CSV group (-> $WORK/csv).
# $1=binary $2=workload-forces $3=event-group $4=per_lane
runperf(){
  local bin="$1" forces="$2" group="$3" pl="$4" args=() f
  IFS=',' read -ra F <<<"$forces"; for f in "${F[@]}"; do args+=(--force "$f"); done
  taskset -c "$CORE" perf stat -x, -e "$group" -- \
    "$bin" "${args[@]}" --lanes "$LANES" --per-lane "$pl" --seed "$SEED" \
    >"$WORK/out" 2>"$WORK/csv" || true
}
csv(){ awk -F, -v e="$1" '$3==e{print $1; exit}' "$WORK/csv"; }   # scaled count for event
g_att(){ grep -oP 'yield \d+ / \K\d+' "$WORK/out"; }              # actual attempts (overshoots target)
g_us(){  grep -oP '\K[0-9.]+(?= us/att)' "$WORK/out"; }
fnum(){ awk -v a="$1" -v b="$2" -v s="${3:-1}" 'BEGIN{printf (b==0?0:s*a/b)}'; }

# ───────────────────────── section: TOPDOWN (§2 + LOG) ─────────────────────────
sec_topdown(){
  rule; say "§2 WHOLE-PROGRAM TOP-DOWN  (findpar-bench, ${LANES}×${PER_LANE} att, pinned core $CORE)"
  printf '%-4s %7s %7s %7s %5s | %5s %5s %5s %5s %4s | %4s %5s %6s %5s %6s %5s\n' \
    wl us/att cyc/att Kins/a IPC ret% fe% be% bad% smt% MPKI tkn% mispr% OC-h% L1ld/a L1m%
  local entry label forces
  for entry in "${WORKLOADS[@]}"; do
    label="${entry%%|*}"; forces="${entry#*|}"
    # G1 top-down buckets
    runperf "$BIN_PROD" "$forces" "$G_TOPDOWN" "$PER_LANE"
    local att us cyc ops disp be fe smt slots
    att="$(g_att)"; us="$(g_us)"
    cyc="$(csv cycles)"; ops="$(csv ex_ret_ops)"; disp="$(csv de_src_op_disp.all)"
    be="$(csv de_no_dispatch_per_slot.backend_stalls)"
    fe="$(csv de_no_dispatch_per_slot.no_ops_from_frontend)"
    smt="$(csv de_no_dispatch_per_slot.smt_contention)"
    slots="$(awk -v c="$cyc" 'BEGIN{print 6*c}')"
    local ret_p bad_p be_p fe_p smt_p cyc_a
    ret_p="$(fnum "$ops" "$slots" 100)"
    bad_p="$(awk -v d="$disp" -v o="$ops" -v s="$slots" 'BEGIN{print s==0?0:100*(d-o)/s}')"
    be_p="$(fnum "$be" "$slots" 100)"; fe_p="$(fnum "$fe" "$slots" 100)"; smt_p="$(fnum "$smt" "$slots" 100)"
    cyc_a="$(fnum "$cyc" "$att")"
    # G2 branch/divide
    runperf "$BIN_PROD" "$forces" "$G_BRANCH" "$PER_LANE"
    local ins brn misp tkn ipc kins mpki tkn_p mispr_p
    ins="$(csv instructions)"; brn="$(csv ex_ret_brn)"; misp="$(csv ex_ret_brn_misp)"; tkn="$(csv ex_ret_brn_tkn)"
    ipc="$(fnum "$ins" "$(csv cycles)")"; kins="$(fnum "$ins" "$att" 0.001)"
    mpki="$(fnum "$misp" "$ins" 1000)"; tkn_p="$(fnum "$tkn" "$brn" 100)"; mispr_p="$(fnum "$misp" "$brn" 100)"
    # G3 op-cache / vector
    runperf "$BIN_PROD" "$forces" "$G_OPCACHE" "$PER_LANE"
    local oc_p
    oc_p="$(fnum "$(csv op_cache_hit_miss.op_cache_hit)" "$(csv op_cache_hit_miss.all_op_cache_accesses)" 100)"
    # G4 L1
    runperf "$BIN_PROD" "$forces" "$G_L1" "$PER_LANE"
    local l1ld_a l1m_p
    l1ld_a="$(fnum "$(csv ls_dispatch.ld_dispatch)" "$att")"
    l1m_p="$(fnum "$(csv l2_cache_req_stat.dc_access_in_l2)" "$(csv all_data_cache_accesses)" 100)"
    printf '%-4s %7.1f %7.0f %7.1f %5.2f | %5.1f %5.1f %5.1f %5.1f %4.1f | %4.1f %5.1f %6.2f %5.1f %6.0f %5.2f\n' \
      "$label" "$us" "$cyc_a" "$kins" "$ipc" "$ret_p" "$fe_p" "$be_p" "$bad_p" "$smt_p" \
      "$mpki" "$tkn_p" "$mispr_p" "$oc_p" "$l1ld_a" "$l1m_p"
  done
  say "  (slots = 6×cycles; bad-spec = (de_src_op_disp.all − ex_ret_ops)/slots; the five"
  say "   buckets sum to ~100%. OC-h% = op-cache hit rate; L1m% = DC reqs that miss to L2.)"
}

# ───────────────────────── section: UTIL (§3) ─────────────────────────
sec_util(){
  rule; say "§3 UNIFIED-WARP UTILIZATION  (findpar-bench --features count)"
  printf '%-4s %8s %10s %10s\n' wl LANES util passes/att
  local entry label forces f args
  for entry in "${WORKLOADS[@]}"; do
    label="${entry%%|*}"; forces="${entry#*|}"; args=()
    IFS=',' read -ra F <<<"$forces"; for f in "${F[@]}"; do args+=(--force "$f"); done
    taskset -c "$CORE" "$BIN_COUNT" "${args[@]}" --lanes "$LANES" --per-lane "$PER_LANE" --seed "$SEED" >"$WORK/out" 2>/dev/null || true
    local lc util ppa
    lc="$(grep -oP 'LANES=\K[0-9]+' "$WORK/out" | head -1)"
    util="$(grep -oP 'util \K[0-9.]+' "$WORK/out" | head -1)"
    ppa="$(grep -oP 'passes/att \K[0-9.]+' "$WORK/out" | head -1)"
    printf '%-4s %8s %10s %10s\n' "$label" "${lc:-?}" "${util:-?}" "${ppa:-?}"
  done
  say "  (DEEPDIVE: util ~0.998, ~77 warp passes/att — the warp is ~always full.)"
}

# ───────────────────────── section: SELFTIME (§4) ─────────────────────────
sec_selftime(){
  rule; say "§4 PER-SYMBOL SELF-TIME  (perf record, cycles; self% per named region)"
  printf '%-16s' region; local entry l f; for entry in "${WORKLOADS[@]}"; do printf '%6s' "${entry%%|*}"; done; printf '\n'
  declare -A V
  for entry in "${WORKLOADS[@]}"; do
    local label="${entry%%|*}" forces="${entry#*|}" args=()
    IFS=',' read -ra F <<<"$forces"; for f in "${F[@]}"; do args+=(--force "$f"); done
    taskset -c "$CORE" perf record -o "$WORK/perf.data" -e cycles -- \
      "$BIN_PROD" "${args[@]}" --lanes "$LANES" --per-lane "$PER_LANE" --seed "$SEED" >/dev/null 2>&1 || true
    perf report -i "$WORK/perf.data" --stdio --sort symbol --percent-limit 0 2>/dev/null >"$WORK/rep" || true
    local sym; for sym in "${SELFTIME_SYMS[@]}"; do
      local rl="${sym%%|*}" pat="${sym#*|}"
      # sum self-% (col 1, e.g. "25.37%") over every symbol containing the pattern
      V["$rl,$label"]="$(awk -v p="$pat" '!/^#/ && index($0,p){g=$1;gsub(/%/,"",g);s+=g} END{printf "%.1f",s}' "$WORK/rep")"
    done
  done
  for sym in "${SELFTIME_SYMS[@]}"; do
    local rl="${sym%%|*}"; printf '%-16s' "$rl"
    for entry in "${WORKLOADS[@]}"; do printf '%6s' "${V["$rl,${entry%%|*}"]:-0.0}"; done; printf '\n'
  done
  say "  (RAW self-time. Mapping to DEEPDIVE 'regions' — inlined clear_clue, the 'subset"
  say "   family' aggregate — and the binding-term column are the HUMAN/AGENT step.)"
}

# ───────────────────────── LANES flip (lanes16 / sde) ─────────────────────────
ORIG_LANES_LINE=""
restore_lanes(){ [[ -n "$ORIG_LANES_LINE" ]] && \
  sed -i "s/^pub const LANES: usize = .*/$ORIG_LANES_LINE/" "$LANES_FILE" && ORIG_LANES_LINE=""; }
set_lanes(){ # N
  ORIG_LANES_LINE="$(grep -oP '^pub const LANES: usize = .*' "$LANES_FILE")"
  sed -i "s/^pub const LANES: usize = .*/pub const LANES: usize = $1;/" "$LANES_FILE"
}
trap restore_lanes EXIT

# ───────────────────────── section: LANES16 ─────────────────────────
sec_lanes16(){
  rule; say "LANES16: 8-wide (ymm) vs 16-wide (zmm) unified warp — shape + double-pump"
  local b8 b16
  set_lanes 8;  b8="$(build_findpar_bench '' l8)"
  set_lanes 16; b16="$(build_findpar_bench '' l16)"; restore_lanes
  # objdump ymm/zmm/xmm operand counts inside warp_pass_full (any monomorph)
  say "  warp_pass_full operand-register-width counts (objdump, in-function):"
  local bn
  for bn in "l8:$b8" "l16:$b16"; do
    local tag="${bn%%:*}" path="${bn#*:}"
    objdump -d --demangle "$path" 2>/dev/null | awk '
      /warp_pass_full/ && /:$/ {inf=1}
      inf && /^[0-9a-f]+ <.*>:$/ && !/warp_pass_full/ {inf=0}
      inf { for(i=1;i<=NF;i++){ if($i ~ /%ymm/)y++; else if($i ~ /%zmm/)z++; else if($i ~ /%xmm/)x++ } }
      END{ printf "    %-4s ymm=%-5d zmm=%-5d xmm=%-5d\n", t, y, z, x }' t="$tag"
  done
  # top-down + double-pump per workload, both widths
  printf '  %-4s %-4s %8s %8s %5s %6s %6s %9s\n' wl cfg cyc/att Kins/a IPC be% bad% vecops/a
  local entry label forces
  for entry in "${WORKLOADS[@]}"; do
    label="${entry%%|*}"; forces="${entry#*|}"
    for cfg in "8:$b8" "16:$b16"; do
      local w="${cfg%%:*}" bin="${cfg#*:}"
      runperf "$bin" "$forces" "$G_TOPDOWN" "$PER_LANE"
      local att cyc ops disp be slots
      att="$(g_att)"; cyc="$(csv cycles)"; ops="$(csv ex_ret_ops)"; disp="$(csv de_src_op_disp.all)"
      be="$(csv de_no_dispatch_per_slot.backend_stalls)"; slots="$(awk -v c="$cyc" 'BEGIN{print 6*c}')"
      runperf "$bin" "$forces" "$G_OPCACHE" "$PER_LANE"   # vector ops + instructions reuse
      local vec ins
      vec="$(csv sse_avx_ops_retired.all)"
      runperf "$bin" "$forces" "$G_BRANCH" "$PER_LANE"; ins="$(csv instructions)"
      printf '  %-4s %-4s %8.0f %8.1f %5.2f %6.1f %6.1f %9.0f\n' \
        "$label" "$w" "$(fnum "$cyc" "$att")" "$(fnum "$ins" "$att" 0.001)" \
        "$(fnum "$ins" "$cyc")" "$(fnum "$be" "$slots" 100)" \
        "$(awk -v d="$disp" -v o="$ops" -v s="$slots" 'BEGIN{print s==0?0:100*(d-o)/s}')" \
        "$(fnum "$vec" "$att")"
    done
  done
  say "  (Double-pump: l16 retires ~40% fewer vec ops/att yet cyc/att is ~flat -> 512-bit"
  say "   ops are 2×256-bit µops on Zen4. Verdict in the doc is the HUMAN step.)"
}

# ───────────────────────── section: SDE (-mix) ─────────────────────────
# The register-width partition comes from SDE's own *isa-set histogram, whose names
# encode operand width (AVX512F_256=ymm, _512=zmm, _128/SSE=xmm, *KOP*=kmask). It sums
# EXACTLY to SDE's *total. Plain VEX AVX/AVX2 ops are width-unsuffixed in the isa-set
# name, so they get their own honest 'vexA' bucket (the DEEPDIVE folded these into
# ymm/xmm by hand). We read the FIRST EMIT_DYNAMIC_STATS block (single-thread = whole
# program); the later per-function copies repeat the same lines and must be excluded.
mix_block1(){ awk '/# EMIT_DYNAMIC_STATS/{b++} b==1{print} b==1 && /# END_DYNAMIC_STATS/{exit}' "$1"; }
sec_sde(){
  rule; say "SDE -mix: instruction shape (isa-set width partition + top opcodes), l8 vs l16"
  [[ -x "$SDE" ]] || { say "  SDE not found at '$SDE' (set \$SDE) — skipping."; return; }
  local att=$((LANES*PER_LANE_SDE)) b8 b16
  set_lanes 8;  b8="$(build_findpar_bench '' l8)"
  set_lanes 16; b16="$(build_findpar_bench '' l16)"; restore_lanes
  local entry label forces cfg
  for entry in "${WORKLOADS[@]}"; do
    label="${entry%%|*}"; forces="${entry#*|}"
    say "  --- $label (${LANES}×${PER_LANE_SDE}=$att att) ---"
    printf '    %-3s %9s %8s %8s %8s %7s %7s %9s\n' cfg total/a zmm/a ymm/a xmm/a kmsk/a vexA/a scalar/a
    for cfg in "8:$b8" "16:$b16"; do
      local w="${cfg%%:*}" bin="${cfg#*:}"
      local mix="$WORK/mix-$label-$w.out" b1="$WORK/mix-$label-$w.b1" args=() f
      IFS=',' read -ra F <<<"$forces"; for f in "${F[@]}"; do args+=(--force "$f"); done
      taskset -c "$CORE" "$SDE" -mix -omix "$mix" -- \
        "$bin" "${args[@]}" --lanes "$LANES" --per-lane "$PER_LANE_SDE" --seed "$SEED" >/dev/null 2>&1 || true
      [[ -f "$mix" ]] || { say "    $w: no mix output"; continue; }
      mix_block1 "$mix" >"$b1"
      awk -v a="$att" -v w="$w" '
        /^\*isa-set-/ { n=$1; c=$NF+0
          if      (n ~ /_512/)           z+=c
          else if (n ~ /_256/)           y+=c
          else if (n ~ /_128|SSE/)       x+=c
          else if (n ~ /KOP/)            k+=c
          else if (n ~ /isa-set-AVX2?$/) v+=c
          else                           s+=c }
        /^\*total/ { t=$NF+0 }
        END{printf "    %-3s %9.0f %8.0f %8.0f %8.0f %7.0f %7.0f %9.0f\n", w, t/a,z/a,y/a,x/a,k/a,v/a,s/a}
      ' "$b1"
    done
    say "    top opcodes (count/att, l8 | l16):"
    paste \
      <(awk '/^[A-Z][A-Z0-9_]+ +[0-9]+$/{print $1,$2}' "$WORK/mix-$label-8.b1"  2>/dev/null | sort -k2 -nr | head -10) \
      <(awk '/^[A-Z][A-Z0-9_]+ +[0-9]+$/{print $1,$2}' "$WORK/mix-$label-16.b1" 2>/dev/null | sort -k2 -nr | head -10) \
      | awk -v a="$att" '{printf "      %-12s %9.0f   |   %-12s %9.0f\n",$1,$2/a,$3,$4/a}'
  done
  say "  (Partition sums to SDE *total exactly. The AVX512F_256→_512 isa-set flip l8→l16"
  say "   IS the double-pump made visible; vexA = VEX ops SDE leaves width-unsuffixed.)"
}

# ───────────────────────── main ─────────────────────────
rule; say "DEEPDIVE numeric reproduction — $(LC_ALL=C date)"
say "crate=$CRATE_DIR  core=$CORE  per_lane=$PER_LANE  quick=$QUICK  sections=${SECTIONS[*]}"
have perf || { echo "perf not found" >&2; exit 1; }
setup_env

BIN_PROD="$(build_findpar_bench '' prod)"
BIN_COUNT="$(build_findpar_bench count count)"
say "built: $(basename "$BIN_PROD"), $(basename "$BIN_COUNT")"

for s in "${SECTIONS[@]}"; do
  case "$s" in
    topdown)  sec_topdown ;;
    util)     sec_util ;;
    selftime) sec_selftime ;;
    lanes16)  sec_lanes16 ;;
    sde)      sec_sde ;;
  esac
done

rule
say "NOT REPRODUCED HERE — needs an agent/human (judgment, not measurement):"
say "  • §5 instruction-level dossiers: identify hot lines in disasm (the CellMarks::get"
say "    bounds check; the vzeroupper / kmovd SKIDS that must be discounted, not counted)."
say "  • Binding-term verdict per region; the §2 throughput cost model."
say "  • Map per-symbol self-time onto named regions (inlined clear_clue, 'subset family')."
say "  • §6 memory reconciliation; §7 ranked optimization candidates + risk."
say "Feed the tables above to an agent to regenerate the DEEPDIVE prose with current names."
rule
rm -rf "$WORK"
