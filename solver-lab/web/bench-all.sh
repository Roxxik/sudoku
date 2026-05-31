#!/usr/bin/env bash
# Build the solver-lab wasm (simd128) and run the bench under every wasm engine
# available here: V8 (node = mobile Chrome's engine) and SpiderMonkey (js140 =
# Firefox's engine), plus wasmtime (Cranelift) for reference. All are x86 hosts;
# mobile is ARM/NEON, so treat these as the engine comparison, not the final ISA
# word — validate top candidates on a real device.
#
# Usage: solver-lab/web/bench-all.sh [attempts=4000] [seed=1]
set -euo pipefail
cd "$(dirname "$0")/../.."

ATTEMPTS="${1:-4000}"
SEED="${2:-1}"

echo "building wasm (cdylib, +simd128) and wasip1 example..."
cargo build --release -p solver-lab --target wasm32-unknown-unknown >/dev/null 2>&1
cargo build --release -p solver-lab --example bench --target wasm32-wasip1 >/dev/null 2>&1

echo
echo "### V8 / node (== mobile Chrome engine)"
node solver-lab/web/bench.mjs "$ATTEMPTS" "$SEED"

echo
echo "### SpiderMonkey / js140 (== Firefox engine)"
js140 solver-lab/web/bench-sm.js "$ATTEMPTS" "$SEED"

echo
echo "### wasmtime / Cranelift (reference only)"
wasmtime run target/wasm32-wasip1/release/examples/bench.wasm --attempts "$ATTEMPTS" --seed "$SEED"
