#!/usr/bin/env bash
# Low-noise paired benchmark harness for the generator attempt path.
# Usage: ./measure.sh <label> [seeds=10] [attempts=20000] [reps=3] [core=2]
#
# For each seed in 1..seeds it runs the bench `reps` times (pinned to one core,
# performance governor assumed) and keeps the MIN us/attempt per mode (min is the
# noise-resistant estimator for CPU-bound work). Writes per-seed mins + fp to
# measure_<label>.tsv and prints a summary. Run the same way before/after a change
# and diff the two tsv files seed-by-seed (paired) — the fp column must match per
# (seed) or the variant changed the work it does.
set -euo pipefail
cd "$(dirname "$0")/.."

label=${1:?usage: measure.sh <label> [seeds] [attempts] [reps] [core]}
seeds=${2:-10}
att=${3:-20000}
reps=${4:-3}
core=${5:-2}
out="generator-lab/measure_${label}.tsv"

cargo build --release -q -p generator-lab --example bench
bin=./target/release/examples/bench

echo "# label=$label seeds=$seeds attempts=$att reps=$reps core=$core gov=$(cat /sys/devices/system/cpu/cpufreq/policy0/scaling_governor)" >"$out"
printf "%s\t%s\t%s\t%s\t%s\n" seed train_us drill_us train_fp drill_fp >>"$out"

# warmup (discarded)
taskset -c "$core" "$bin" --attempts "$att" --seed 1 >/dev/null 2>&1

printf "%-5s %10s %10s\n" seed train_us drill_us
ts=0; ds=0
for seed in $(seq 1 "$seeds"); do
  tmin=""; dmin=""; tfp=""; dfp=""
  for _ in $(seq 1 "$reps"); do
    line=$(taskset -c "$core" "$bin" --attempts "$att" --seed "$seed" 2>/dev/null)
    read -r tu tf <<<"$(awk '/^train/{fp=$NF;gsub(/[()]/,"",fp);print $3,fp}' <<<"$line")"
    read -r du df <<<"$(awk '/^drill/{fp=$NF;gsub(/[()]/,"",fp);print $3,fp}' <<<"$line")"
    tmin=$(awk -v a="$tu" -v b="$tmin" 'BEGIN{print (b==""||a<b)?a:b}')
    dmin=$(awk -v a="$du" -v b="$dmin" 'BEGIN{print (b==""||a<b)?a:b}')
    tfp=$tf; dfp=$df
  done
  printf "%-5s %10s %10s\n" "$seed" "$tmin" "$dmin"
  printf "%s\t%s\t%s\t%s\t%s\n" "$seed" "$tmin" "$dmin" "$tfp" "$dfp" >>"$out"
  ts=$(awk -v s="$ts" -v a="$tmin" 'BEGIN{print s+a}')
  ds=$(awk -v s="$ds" -v a="$dmin" 'BEGIN{print s+a}')
done

awk -v ts="$ts" -v ds="$ds" -v n="$seeds" 'BEGIN{printf "\nmean-of-seed-mins  train %.2f us  drill %.2f us\n", ts/n, ds/n}'
echo "wrote $out"
