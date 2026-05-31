// Cross-engine benchmark runner for the generator-lab wasm module (node / V8,
// the engine in mobile Chrome). Loads the wasm32-unknown-unknown cdylib (built
// with +simd128) and times train(HiddenQuad) and drill(HiddenQuad) generation
// for a fixed number of attempts with performance.now() — exactly how a browser
// measures. The same module + index.html runs in a real browser for ARM/mobile.
//
// Usage: node generator-lab/web/bench.mjs [attempts=1500] [seed=1] [mode=both|train|drill]

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const attempts = Number(process.argv[2] ?? 1500);
const seed = Number(process.argv[3] ?? 1);
const which = process.argv[4] ?? "both";

const here = dirname(fileURLToPath(import.meta.url));
const wasmPath = join(
  here,
  "../../target/wasm32-unknown-unknown/release/generator_lab.wasm",
);

const bytes = readFileSync(wasmPath);
const { instance } = await WebAssembly.instantiate(bytes, {});
const ex = instance.exports;

const MODES = [
  ["train", 0],
  ["drill", 1],
].filter(([name]) => which === "both" || which === name);

console.log(
  `generator-lab wasm bench (V8 ${process.versions.v8}): ${attempts} attempts, seed ${seed}\n`,
);
console.log(
  ["mode".padEnd(7), "ms".padStart(9), "us/attempt".padStart(12), "puzzles".padStart(9), "atts/puzzle".padStart(13)].join(" "),
);

for (const [name, mode] of MODES) {
  // Warm up the JIT (TurboFan tiers up after the first run), then measure.
  ex.bench(mode, Math.min(attempts, 400), seed);
  const t0 = performance.now();
  ex.bench(mode, attempts, seed);
  const ms = performance.now() - t0;
  const puzzles = ex.bench_yield(mode, attempts, seed) >>> 0;

  const us = (ms * 1000) / attempts;
  const attsPer = puzzles > 0 ? (attempts / puzzles).toFixed(0) : "inf";
  console.log(
    [name.padEnd(7), ms.toFixed(1).padStart(9), us.toFixed(1).padStart(12), String(puzzles).padStart(9), attsPer.padStart(13)].join(" "),
  );
}
