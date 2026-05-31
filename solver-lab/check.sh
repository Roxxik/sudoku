#!/usr/bin/env bash
# solver-lab/check.sh — one-stop after adding/registering a prover variant.
#
#   1. Correctness: every registered prober vs the oracle, plus baseline
#      faithfulness vs core. Aborts here on any mismatch.
#   2. Build the +simd128 wasm and copy it to web/, so serve.sh and the mobile
#      page pick up the new variants automatically (the JS discovers them from
#      the module — no JS edit needed).
#   3. Benches: native, then the wasm bench under whatever engines are installed
#      (V8/node, SpiderMonkey/js140, wasmtime). All read the same wasm built in
#      step 2.
#
# Usage: solver-lab/check.sh [attempts=4000] [seed=1]
set -euo pipefail
cd "$(dirname "$0")/.."   # repo root (this script lives in solver-lab/)

ATTEMPTS="${1:-4000}"
SEED="${2:-1}"
WASM=target/wasm32-unknown-unknown/release/solver_lab.wasm

echo "== 1/3  correctness (every registered prober vs oracle) =="
cargo test -p solver-lab --release

echo
echo "== 2/3  build web wasm (+simd128) =="
cargo build --release -p solver-lab --target wasm32-unknown-unknown \
  --config 'profile.release.strip="debuginfo"' >/dev/null
cp "$WASM" solver-lab/web/solver_lab.wasm
echo "  web/solver_lab.wasm refreshed ($(du -h solver-lab/web/solver_lab.wasm | cut -f1))"

echo
echo "== 3/3  benches ($ATTEMPTS attempts, seed $SEED) =="
echo "--- native ---"
cargo run --release -p solver-lab --example bench -- --attempts "$ATTEMPTS" --seed "$SEED"

if command -v node >/dev/null 2>&1; then
  echo
  echo "--- V8 / node (== mobile Chrome engine) ---"
  node solver-lab/web/bench.mjs "$ATTEMPTS" "$SEED"
fi
if command -v js140 >/dev/null 2>&1; then
  echo
  echo "--- SpiderMonkey / js140 (== Firefox engine) ---"
  js140 solver-lab/web/bench-sm.js "$ATTEMPTS" "$SEED"
fi
if command -v wasmtime >/dev/null 2>&1; then
  echo
  echo "--- wasmtime / Cranelift (reference) ---"
  cargo build --release -p solver-lab --example bench --target wasm32-wasip1 >/dev/null 2>&1
  wasmtime run target/wasm32-wasip1/release/examples/bench.wasm --attempts "$ATTEMPTS" --seed "$SEED"
fi

echo
echo "done. Desktop x86 wasm is a POOR proxy for ARM — for real-device numbers run:"
echo "    solver-lab/web/serve.sh"
