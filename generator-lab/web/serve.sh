#!/usr/bin/env bash
# Build the stripped generator-lab wasm and serve the mobile bench page over the
# LAN, so you can open it on a phone (same wifi) in Chrome and Firefox and run
# train/drill on real ARM. Foreground server — Ctrl-C to stop.
#
# Usage: generator-lab/web/serve.sh [port=8000]
set -euo pipefail
cd "$(dirname "$0")/../.."

PORT="${1:-8000}"

echo "building stripped wasm (+simd128)…"
cargo build --release -p generator-lab --target wasm32-unknown-unknown \
  --config 'profile.release.strip="debuginfo"' >/dev/null 2>&1
cp target/wasm32-unknown-unknown/release/generator_lab.wasm generator-lab/web/generator_lab.wasm
echo "wasm: $(du -h generator-lab/web/generator_lab.wasm | cut -f1)"
echo
echo "Open on your phone (same wifi):"
for ip in $(hostname -I 2>/dev/null); do echo "    http://$ip:$PORT/"; done
echo
echo "Reported results print here live and append to generator-lab/web/results.jsonl."
echo
exec python generator-lab/web/serve.py "$PORT"
