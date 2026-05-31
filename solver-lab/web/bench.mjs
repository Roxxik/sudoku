// Cross-engine benchmark runner for the solver-lab wasm module.
//
// Loads the wasm32-unknown-unknown cdylib (built with +simd128), discovers the
// registered prober variants from the module itself (variant_count /
// variant_name), runs each for `attempts` strip attempts, and times it with
// performance.now() — exactly how a browser measures. Runs under node (V8, the
// engine in mobile Chrome) today; the same module + a tiny HTML page runs in a
// real browser for ARM/mobile validation.
//
// Variants are discovered at runtime, so registering a prover in Rust
// (solvers::register!) needs no edit here.
//
// Usage: node solver-lab/web/bench.mjs [attempts=4000] [seed=1]

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const attempts = Number(process.argv[2] ?? 4000);
const seed = Number(process.argv[3] ?? 1);

const here = dirname(fileURLToPath(import.meta.url));
const wasmPath = join(
  here,
  "../../target/wasm32-unknown-unknown/release/solver_lab.wasm",
);

const bytes = readFileSync(wasmPath);
const { instance } = await WebAssembly.instantiate(bytes, {});
const ex = instance.exports;

// Discover variant names straight from the module's data segment.
const dec = new TextDecoder();
function variantName(i) {
  const ptr = ex.variant_name_ptr(i);
  const len = ex.variant_name_len(i);
  return dec.decode(new Uint8Array(ex.memory.buffer, ptr, len));
}
const count = ex.variant_count();
const variants = Array.from({ length: count }, (_, i) => [variantName(i), i]);

console.log(
  `solver-lab wasm bench (V8 ${process.versions.v8}): ${attempts} attempts/variant, seed ${seed}`,
);
console.log(`${count} variants registered\n`);
console.log(
  ["variant".padEnd(14), "ms".padStart(9), "us/attempt".padStart(12), "vs base".padStart(9)].join(" "),
);

let baseMs = null;
let baseFp = null;
for (const [name, idx] of variants) {
  // Warm up the JIT (TurboFan tiers up after the first run), then measure.
  ex.bench(idx, Math.min(attempts, 500), seed);
  const t0 = performance.now();
  const fp = ex.bench(idx, attempts, seed);
  const ms = performance.now() - t0;

  if (baseFp === null) baseFp = fp;
  else if (fp !== baseFp) {
    console.error(`FINGERPRINT MISMATCH on ${name}: ${fp} != ${baseFp}`);
    process.exit(1);
  }

  const speedup = baseMs === null ? 1 : baseMs / ms;
  if (baseMs === null) baseMs = ms;
  const us = (ms * 1000) / attempts;
  console.log(
    [name.padEnd(14), ms.toFixed(1).padStart(9), us.toFixed(1).padStart(12), (speedup.toFixed(2) + "x").padStart(9)].join(" "),
  );
}
