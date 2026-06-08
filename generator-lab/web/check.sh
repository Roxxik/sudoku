#!/usr/bin/env bash
# generator-lab/check.sh — build + bench in one step.
#
#   1. Build the native examples and the +simd128 wasm; copy the wasm to web/ so
#      serve.sh and the mobile page pick it up.
#   2. Native bench (train + drill, fixed attempts).
#   3. wasm bench under whatever engines are installed (V8/node, SpiderMonkey/js140,
#      wasmtime). All read the same wasm built in step 1.
#
# Real-device ARM numbers (the deployment target) need a phone — run:
#     generator-lab/web/serve.sh
#
# Usage: generator-lab/check.sh [attempts=2000] [seed=1]
set -euo pipefail
cd "$(dirname "$0")/.."   # repo root (this script lives in generator-lab/)

ATTEMPTS="${1:-2000}"
SEED="${2:-1}"
WASM=target/wasm32-unknown-unknown/release/generator_lab.wasm

echo "== 1/3  build (native examples + +simd128 wasm) =="
cargo build --release -p generator-lab --example bench --example find >/dev/null
cargo build --release -p generator-lab --target wasm32-unknown-unknown \
  --config 'profile.release.strip="debuginfo"' >/dev/null
cp "$WASM" generator-lab/web/generator_lab.wasm
echo "  web/generator_lab.wasm refreshed ($(du -h generator-lab/web/generator_lab.wasm | cut -f1))"

echo
echo "== 2/3  native bench ($ATTEMPTS attempts, seed $SEED) =="
cargo run --release -q -p generator-lab --example bench -- --attempts "$ATTEMPTS" --seed "$SEED"

echo
echo "== 3/3  wasm benches =="
if command -v node >/dev/null 2>&1; then
  echo "--- V8 / node (== mobile Chrome engine) ---"
  node generator-lab/web/bench.mjs "$ATTEMPTS" "$SEED"
fi
if command -v js140 >/dev/null 2>&1; then
  echo
  echo "--- SpiderMonkey / js140 (== Firefox engine) ---"
  js140 generator-lab/web/bench-sm.js "$ATTEMPTS" "$SEED"
fi
if command -v wasmtime >/dev/null 2>&1; then
  echo
  echo "--- wasmtime / Cranelift (reference) ---"
  cargo build --release -p generator-lab --example bench --target wasm32-wasip1 >/dev/null 2>&1 || true
  wasmtime run target/wasm32-wasip1/release/examples/bench.wasm --attempts "$ATTEMPTS" --seed "$SEED" 2>/dev/null || true
fi

echo
echo "done. Desktop x86 wasm is a POOR proxy for ARM — for real-device numbers run:"
echo "    generator-lab/web/serve.sh"
