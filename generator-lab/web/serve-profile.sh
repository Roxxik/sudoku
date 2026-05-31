#!/usr/bin/env bash
# Build the PROFILING wasm (name section kept, phase-boundary functions marked
# #[inline(never)] via the `profiling` feature) and serve the bench page, so a
# real-device browser profiler (Chrome chrome://inspect / Firefox
# about:debugging over USB) attributes samples to distinct frames —
# random_full_grid / clear_naked / apply_clear / apply_place / any_alt_solves /
# solve_first / baseline / verify — instead of one giant `attempt`.
#
# NOTE: this build carries ~3% call-overhead on the instrumented phases, so use
# `serve.sh` (not this) for clean timing numbers. Same code/behaviour otherwise.
#
# Usage: generator-lab/web/serve-profile.sh [port=8000]
set -euo pipefail
cd "$(dirname "$0")/../.."

PORT="${1:-8000}"

echo "building PROFILING wasm (+simd128, names, no phase inlining)…"
cargo build --release -p generator-lab --target wasm32-unknown-unknown --features profiling \
  --config 'profile.release.strip="debuginfo"' >/dev/null 2>&1
cp target/wasm32-unknown-unknown/release/generator_lab.wasm generator-lab/web/generator_lab.wasm
echo "wasm: $(du -h generator-lab/web/generator_lab.wasm | cut -f1) (profiling build)"
echo
echo "Open on your phone (same wifi), then attach the profiler over USB:"
for ip in $(hostname -I 2>/dev/null); do echo "    http://$ip:$PORT/"; done
echo "  Chrome:  chrome://inspect on the laptop -> inspect the tab -> Performance -> Record"
echo "  Firefox: about:debugging -> This Firefox/USB -> the tab -> Performance"
echo "  Then tap Run train / Run drill on the phone while recording."
echo
exec python generator-lab/web/serve.py "$PORT"
